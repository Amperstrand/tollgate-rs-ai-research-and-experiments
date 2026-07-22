# Issue #60: Cloud Lab Test Skip Investigation

## Summary

Investigation of test skips in the Physical Router Test Automation (PRTA)
project at `/home/ubuntu/src/physical-router-test-automation/tests/`.

## Skip Counts

| Mechanism | Count | Location |
|-----------|-------|----------|
| `pytest.skip()` runtime calls | 495 | Across all test files |
| `pytest.mark.skipif` decorators | 9 | `tests/api/test_portal_verify.py` |
| `pytest.mark.skip` (hardware in virtual lab) | 1 | `tests/conftest.py:746` |
| Hardware scenario test files (auto-skipped in virtual lab) | 17 | `tests/scenarios/` |
| **Total test files** | 151 | `tests/` |

## Categorization by Reason

### 1. Missing CLI Socket / Backend Support (~50+ skips)
Tests check for `/var/run/tollgate.sock` CLI socket and skip when the
backend doesn't support it. Primarily in `test_mint_health.py`,
`test_cli_json_config.py`, `test_keyset_id_versions.py`.
Reason: "CLI socket not supported by this backend"

### 2. Hardware Scenarios in Virtual Lab (17 test files)
The entire `tests/scenarios/` directory is marked `hardware`. When
`TOLLGATE_VIRTUAL_LAB` env var is set, any test with the `hardware`
marker that lacks a `virtual_lab` marker is skipped via
`conftest.py:746-755`. Affected: `test_boot_hygiene.py`,
`test_captive_portal_browser.py`, `test_captive_portal_cashu_payment.py`,
`test_mint_health.py`, `test_multihop_cloud.py`, `test_net4sats_ux.py`,
`test_recovery.py`, `test_reseller_mode.py`, `test_rfc1918_isolation.py`,
`test_router_identity_script.py`, `test_two_router.py`,
`test_two_router_cloud.py`, `test_two_router_payment.py`, `test_upgrade.py`,
`test_upstream_wifi.py`, `test_vendor_ie.py`.
Reason: "hardware scenarios not run in virtual lab / cloud worker"

### 3. Portal Type Mismatch (8 skipif decorators)
Tests in `test_portal_verify.py` skip based on `PORTAL_TYPE` env var
(builtin vs net4sats portal). 4 skip when not builtin, 4 when not net4sats.
Reason: "only for builtin portal" / "only for net4sats portal"

### 4. Cashu Venv / Mint Dependency (~20+ skips)
Tests requiring a cashu venv or mint health tracker that isn't available
in the cloud lab. Primarily in `test_session_expiry_and_scan.py`,
`test_mint_wallet_compat.py`.
Reason: "cashu venv not available — run scripts/setup-cashu.sh"

### 5. Degraded Mode / Service State (~30+ skips)
Tests that skip when the service isn't in the expected operational state
(not running as full merchant, not in degraded mode, etc.). Primarily in
`test_degraded_mode.py`, `test_merchant_provider.py`.
Reason: "Service not running as full merchant" / "Service did not enter degraded mode"

### 6. FIPS Exit Node / VPS Unreachable (~15 skips)
Tests requiring SSH access to a FIPS exit node VPS; skip when the VPS
is unreachable. In `test_fips_exit_node.py`.
Reason: "exit node SSH unreachable"

### 7. Feature Not Yet Deployed (~20+ skips)
Tests for features from pending PRs that skip when the feature doesn't
exist yet (session expiry, mint health tracking signals).
Reason: various, referencing specific PRs/features

### 8. Rust v1 Format Divergence (Issue #42) (~5+ skips)
Tests referencing `Amperstrand/tollgate-rs#42` — Rust v1 session API
format differs from Go. These skips should resolve once the v1 compat
fixes in this commit land. In `test_rust_v1_api.py`, `test_rust_basic_*`.
Reason: "Rust v1 session API format differs from Go (Amperstrand/tollgate-rs#42)"

### 9. mac80211 HWSIM (27 skips)
Tests requiring the `mac80211_hwsim` kernel module for virtual WiFi
interfaces. Skipped when the module isn't loaded (common in cloud labs).
Reason: "mac80211_hwsim not available"

## Conclusion

The vast majority of skips fall into three buckets:
1. **Hardware-specific tests** that can't run in a virtual/cloud lab
2. **Feature-gated tests** for capabilities not yet deployed to the router
3. **Environment-dependent tests** requiring specific infrastructure (cashu venv,
   CLI socket, VPS access, mac80211_hwsim)

Bucket #8 (Rust v1 format divergence) is directly addressed by the fixes
in Issues #42 and #69 in this commit.

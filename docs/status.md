# Status — TollGate alignment & delivery-contract work

*Updated 2026-11-15. The experiments fork is the active home for the
Meter/Delivery design line; canonical vocabulary in
[the glossary](design/glossary.md).*

## Landed (2026-09-12 → 09-13)

| Piece | What | State |
|---|---|---|
| Design notes ×4 | meter-delivery-modularity · non-fungible-minutes · ecash-as-futures · alignment-roadmap | ✅ `docs/design/experiments/` |
| Glossary | Rosetta stone across tollgate-rs / firmware / pecan / provider SDKs + naming rules | ✅ linked from all repos' READMEs |
| `tollgate-core::delivery` | Grant/Caps/Lease, timestamped Progress, 7-kind TerminalReport, BudgetGuard, DeliveryEngine (sans-IO, no_std) | ✅ **42/42 tests** incl. crash-resume equivalence |
| `tollgate-charger-host` | Canonical driver translating the charger MQTT contract + in-process conformance mock | ✅ **7/7 conformance scenarios** |
| Broker-parity tier | same scenarios vs the REAL ev-device-sim + mosquitto (`--features real-broker`) | ✅ **7/7 parity, 2026-09-13** |
| P4 external-segment driver | `ExternalProviderPort` + `ExternalDriver` (quote→deposit, poll-as-tick, receipt-as-truth, nonce↔session idempotency, crash resume) vs mock municipal provider | ✅ **7/7, 2026-09-13** |
| Bil i Oslo STOP captured | `POST /api/Ocpi/Commands/Stop {ParkingChargingSessionId,…}` — pending→INVALID; rig shut down cleanly; resume notes written | ✅ **2026-09-13** |
| phoneautomation versioned | tree reconnected to git (was .git-less), sessions committed, streams/logs ignored; sunset assessment written | ✅ **`960387b`** |
| pecan P3 canonical events | ev-charge daemon emits grant/progress/terminal/settle/refund JSONL (`--canonical-events`, additive, legacy untouched) | ✅ **pecan `263f3e9`, 17/17 tests** |
| evmap M1 provider | `bymoslo` MapProvider — revised to SEED mode (owner decision: no live credentials in the worker until ready; snapshot 1,265 sites, status `unknown`, live layer auto-enables via secrets later) | ✅ **evmap `448e06a`** |
| evmap 5-lane review + fixes | cache-hit 502 fixed (proven), demo start allowlist + fail-closed MQTT, timeouts/single-flight, observability on, smoke 11 providers, provenance surface, ACTIVATION-CHECKLIST | ✅ **evmap `2512283`** |
| Charger MQTT contract | canonical mapping of the wire format + known gaps (no meter ticks, silent stop, wall-vs-monotonic) | ✅ `charger-mqtt-contract.md` |
| Provider captures | Two real-world provider APIs fully mapped (parking + charging lifecycles, auth, quotes, receipts) → SDKs + MCP server | ✅ `phoneautomation/…/{parking,bilioslo}/api/` |
| evmap plan | bymøslo provider adapter (M1–M3) + start-action | ✅ `evmap/docs/bymoslo-provider-plan.md` |
| pecan melt-into-anything | DDR contract generalized, parameter tables, TollGate relationship | ✅ `pecan/docs/melt-into-anything.md` |


## Landed (2026-11-10 → 11-15: v1 compat + review-debt campaign)

15 merged PRs covering the v1_compat layer's money paths, Cashu NUT
compliance, OpenWrt packaging, and testing infrastructure. Key items:

| PR | What |
|---|---|
| #96 | Money guards + LN grant flow (exactly-once registry) |
| #98 | Per-deployment wallet seed + portal vendored |
| #99 | 4 Go v1 API contract fixes (VM E2E discovered) |
| #100 | LN amount fix + .apk packaging for OpenWrt 25.12+ |
| #101 | Repeatable VM E2E harness + dual-format doc |
| #102 | OpenWrt 25.x-first (.apk primary, packaging CI lane) |
| #104 | Persistent wallet localstore (NUT-13 counter survives restarts) |
| #105 | NUT spec audit (include_fee fix, 5 NUTs confirmed correct) |
| #106 | NUT-09/13 seed-only wallet restore (E2E disaster-recovery proven) |
| #107 | NUT-07 startup reconciliation (ambiguous receive recovery) |
| #108 | Keyset fee handling (real-mint integration, testnut.cashu.space) |
| #109 | SSRF validation on NUT-07 checkstate (9 blocklist tests) |

## Test posture

- `cargo test --workspace --features v1-compat` → 150+ passed (14 suites)
- `cargo test -p tollgate-core` → 42 passed
- `cargo test -p tollgate-charger-host` → 7 passed (conformance)
- VM E2E harness (QEMU) → package install + portal verified on 24.10 + 25.12
- Real-mint integration (testnut.cashu.space) → LN loop with keyset fees
- Zero warnings, `unsafe_code = deny` respected.

## Open / next (priority order)

1. **Bench E2E** — flash a physical router with the .apk, run the full
   LN payment loop against testnut, verify DHCP-MAC + real gating.
   The only untested layer (all software layers VM-proven).
2. **Payout flow** — `melt_to_lightning_address` unwired on experimental.
   Feature port from the m-branches.
3. **m2 runtime modules** — session API (tollgate-auth is a confirmed
   consumer), NDS valve, wifi/ubus/uci. Owner cherry-pick decision.
4. **MIPS Bootlin** — owner actively working the cross-toolchain path.
5. **pecan P3 event alignment** — canonical Progress/Terminal events.

## Constraints honored

- Firmware untouched (names via mapping doc only — flash at 92–95%, USB-only).
- pecan/evmap/phoneautomation README pointers left uncommitted (their trees
  carry pending work; lines ride along).
- Live-system renames deferred; dedup happens at the seams.

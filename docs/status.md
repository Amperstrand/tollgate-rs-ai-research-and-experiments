# Status — TollGate alignment & delivery-contract work

*Updated 2026-09-13. The experiments fork is the active home for the
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
| Charger MQTT contract | canonical mapping of the wire format + known gaps (no meter ticks, silent stop, wall-vs-monotonic) | ✅ `charger-mqtt-contract.md` |
| Provider captures | Two real-world provider APIs fully mapped (parking + charging lifecycles, auth, quotes, receipts) → SDKs + MCP server | ✅ `phoneautomation/…/{parking,bilioslo}/api/` |
| evmap plan | bymøslo provider adapter (M1–M3) + start-action | ✅ `evmap/docs/bymoslo-provider-plan.md` |
| pecan melt-into-anything | DDR contract generalized, parameter tables, TollGate relationship | ✅ `pecan/docs/melt-into-anything.md` |

## Test posture

- `cargo test -p tollgate-core` → 42 passed
- `cargo test -p tollgate-charger-host` → 7 passed (conformance)
- Zero warnings, `unsafe_code = deny` respected.

## Open / next (priority order)

1. **pecan P3 event alignment** — deposit flow emits canonical
   Progress/Terminal events alongside legacy ledger names. First live-system
   dedup; do it once parity backs the vocabulary.
3. **evmap M1 provider** — read-only charger catalog (1,265 sites, one
   parameterless call) + live EVSE status + parking-cost context. Highest
   user-visible value per hour.
4. **External-segment examples (P4)** — generalize the provider SDKs into a
   provider-neutral poll-as-tick DeliveryAdapter example in this repo.
5. **Exchange-oracle prototype** — the check-in pattern (broad token →
   narrow entitlement) from ecash-as-futures.
6. Housekeeping — merge/rebase decision vs upstream (3 commits behind
   upstream, 85 ahead; no history rewrite without an explicit call);
   terminology lint for CI.

## Constraints honored

- Firmware untouched (names via mapping doc only — flash at 92–95%, USB-only).
- pecan/evmap/phoneautomation README pointers left uncommitted (their trees
  carry pending work; lines ride along).
- Live-system renames deferred; dedup happens at the seams.

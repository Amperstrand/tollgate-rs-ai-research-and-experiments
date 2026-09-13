# Charger MQTT contract (canonical mapping)

*The wire contract of the charger device family — the ESP32 sticks,
the ESPHome sibling, and `ev-device-sim` (the 132-line Node simulator in
pecan `scripts/ev-device-sim.mjs`) all speak byte-parity versions of this.
Mapped to canonical vocabulary per the
[glossary](../glossary.md). Renaming topics is explicitly **not** proposed:
retained messages and deployed consumers make that a breaking change with no
behavioral gain — dedup happens at the seams.*

## Topics and payloads

| Topic | Payload | Direction | Canonical meaning |
|---|---|---|---|
| `charger/atom/status` | `"online"` / `"offline"` (retained, LWT) | device → host | **delivery-agent availability** |
| `charger/{id}/start` | `{"end": <epoch_sec>}` | host → device | **grant** (time-capped: `end` is the lease deadline; units/cost caps are enforced host-side by the Meter module) |
| `charger/{id}/ack` | `"start-acked"` | device → host | **grant-ack** — delivery agent accepted the grant |
| `charger/{id}/done` | `"countdown-finished"` | device → host | **terminal report** — grant window exhausted (`CAP_SECONDS`) |
| `charger/{id}/stop` | `"OFF"` | host → device | **revoke** — the device kills the countdown; notably it sends **no terminal reply** |
| `charger/{id}/aborted` | (button-sim) | device → host | **terminal report** — device/user-initiated stop-early (`COMPLETED`) |

## Known gaps (documented, not fixed here)

1. **No meter ticks.** The device reports no incremental usage; the LED
   countdown is the meter. The Meter module synthesizes `PROGRESS` from the
   countdown clock (elapsed seconds × price vector). A real meter topic
   (`charger/{id}/tick` with cumulative totals) would make progress
   device-attested — worth adding whenever a reflash happens anyway
   (flash is at 92–95%, USB-only).
2. **Stop is silent.** `stop` gets no acknowledgement, so the host settles
   at its journal after a one-tick grace (the lease catches true failures).
3. **Wall clock on the wire, monotonic inside.** `end` is epoch seconds;
   canonical leases are monotonic `Millis`. Translation happens in the host
   shell only.

## Parity

Proven 2026-09-13: all seven conformance scenarios (full window, seconds
cap with silent stop, money-cap clamp, button abort, revoke, crash-resume
against a live countdown, free delivery) pass against the REAL
`ev-device-sim` over mosquitto — `cargo test -p tollgate-charger-host
--features real-broker --test broker_parity` (sim + broker required).

Known mock-vs-real divergence: the real sim **re-acks a `start` for an
already-running device** (kills the old countdown); the mock refuses.
Irrelevant to the scenarios; align the mock when it matters.

## Reference implementations

- `tollgate-charger-host` (this repo): canonical driver + in-process
  conformance mock of this contract.
- `pecan/scripts/ev-device-sim.mjs`: the production simulator.

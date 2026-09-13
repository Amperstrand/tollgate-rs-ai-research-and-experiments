# TollGate ecosystem glossary (Rosetta stone)

*Canonical vocabulary for the shared machine described in several dialects.
Version 1 — 2026-09-13. Lives in this repo; **any repo that introduces a
concept adds its glossary row in the same PR.** Companion notes:
[meter-delivery-modularity.md](experiments/meter-delivery-modularity.md),
[non-fungible-minutes.md](experiments/non-fungible-minutes.md),
[ecash-as-futures.md](experiments/ecash-as-futures.md),
[alignment-roadmap.md](experiments/alignment-roadmap.md).*

## Core contract terms

| Canonical | Definition | tollgate-rs | charger firmware (atom family) | pecan | provider adapters (SDKs) |
|---|---|---|---|---|---|
| **grant-ack** | The delivery agent's acceptance of a grant (wire: `start-acked`). | charger firmware | charger firmware | — | — |
| **agent availability** | Delivery-agent online/offline presence, retained + LWT (wire: `charger/atom/status`). | — | `charger/atom/status` | daemon health | — |
| **grant** | The scoped authorization a trust-holder issues to a delivery agent: caps + lease + nonce. A capability — holder can deliver up to caps, never re-grant. | session/channel open (implicit) | trigger w/ budget (MQTT start) | melt budget + slider start | start request after prestart quote |
| **caps** | `{max_units, max_seconds, max_cost}` — whichever hits first ends delivery. Money cap is first-class (tariff granularity). | channel capacity | budget (units or seconds) | melt amount | quoted window cost bound |
| **lease** | Expiry-if-not-exercised/refreshed terms on a grant. Makes crash recovery self-healing. | channel exhaustion / rollover threshold | connect timeout | quote expiry / refund window | plug/arrival timeout (e.g. pending-session window) |
| **nonce** | Unique grant identifier enabling idempotency (one grant ↔ one external session). | — | device/session id | melt id | request id ↔ provider session id mapping |
| **Delivery module** | The resource agent: receives grants, delivers, reports. Price-blind by design. | (implicit: forwarding plane) | charger relay/driver + meter tick source | daemon's delivery ledger driver | bridge session driver (poll-as-tick) |
| **Meter module** | The trust-holding economic agent: holds payment/deposit, sizes budgets, consumes reports, settles. | node metering + pricing | — (off-device) | mint-side accounting + refund | quote/fee engine + float |
| **PROGRESS report** | Non-terminal accrual update with cumulative totals **and timestamp** (timestamps make tariffs integrable after the fact). | `MeteringReport` | meter ticks (MQTT) | ledger accrual events | session poll (kwh/minutes so far) |
| **terminal report** | Final outcome + totals + receipt. Variants: `COMPLETED`, `CAP_UNITS`, `CAP_SECONDS`, `CAP_COST`, `FAILED`, `REVOKED`, `LEASE_EXPIRED`. | channel close / settle | STOPPED receipt (device or user stop) | STOPPED receipt (validated vs ledger) | stop receipt (final fee = settlement truth) |
| **receipt** | The settlement-evidence payload of a terminal report. For external providers, the provider's own final response — not our estimate. | settle tx | stop confirmation + meter totals | STOPPED receipt + delivery ledger | provider stop-response (fee, energy, times) |
| **journal** | Persisted (grant, last PROGRESS) enabling reconstruction after a crash. | session persistence | device state | delivery ledger | nonce↔session map + last poll |
| **reconciliation** | Post-crash settle at last journalled reading; lease lapse when no report arrives. | crash recovery suite (this fork) | power-loss behavior | locked-refund-on-quote-check | ambiguous-failure retry rules |

## Settlement / instrument terms

| Canonical | Definition | Where it lives |
|---|---|---|
| **deposit (budget B)** | Money locked up-front for an open-ended delivery; sized by quote or worst contiguous window. | pecan melt; adapter float pre-auth |
| **settle (actual A)** | Burn exactly what the receipt says. | pecan burn; fiat charge to float card |
| **change / refund** | B − A returned as ecash (locked quote). Not an edge case — the normal path for stop-early. | pecan refund quote |
| **float** | The bridge's own funds fronting external providers. Caps concurrent grants. | adapter/bridge ops |
| **quote-before-commit** | Price-preview call that sizes a deposit exactly before starting. | provider prestart/fee APIs |
| **rollover / renewal** | Fresh grant/channel before exhaustion at current rates. Channel analog for external segments; spot-energy only, never parking. | tollgate-rs payment channels; adapters re-grant |
| **poll-as-tick / push-as-tick** | The two metering transports behind one interface: provider polling vs device callbacks. | adapter layer |

## Instrument spectrum (ecash-as-futures)

| Term | Meaning |
|---|---|
| **underlying** | What a keyset's unit promises (kWh, minute, NOK, cinema-X-movie, showing, seat). |
| **scoped commodity** | Fungible within a scope (any movie at cinema X). Keyset-per-scope. |
| **class event** | One instance of a scheduled good (a showing). Supply-capped keyset. |
| **unique entitlement** | A specific instance (seat 12A). Serialized/locked token. |
| **exchange oracle** | The check-in pattern: consumes a broad instrument, materializes a narrower one. The only legitimate cross-underlying exchange. |
| **entitlement binding** | Fungible goods deliver through non-fungible channels: plate × zone, EVSE × session — the grant IS the binding. |

## Naming rules

1. New code uses canonical terms; legacy names may alias during transition.
2. Money amounts in a grant are **milli-units** (e.g. `max_cost_millis`) — never floats.
3. Cumulative counters everywhere (self-healing deltas), always with a timestamp.
4. Terminal variants are exhaustive `enum`s — adding one updates this glossary and every match.

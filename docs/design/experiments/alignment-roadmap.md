# Ecosystem alignment roadmap: gradually becoming one TollGate

*Experiment note (2026-09-13), fourth in the series. Companion to
[meter-delivery-modularity.md](meter-delivery-modularity.md),
[non-fungible-minutes.md](non-fungible-minutes.md),
[ecash-as-futures.md](ecash-as-futures.md). Answers: what do we actually
do — research, experiments, refactors — to align the charger firmware,
pecan, evmap, and the provider adapters with tollgate-rs, gradually.*

---

## The inventory (what speaks what today)

| Component | Vocabulary it speaks | Gap to tollgate-rs |
|---|---|---|
| tollgate-rs core | metering, channels, pricing, access control; `MeteringReport` | no adapter traits yet; resource-agnostic logic only |
| Charger firmware (atom-charger-display family: charger-stick-slint, tdisplay-s3, ESPHome sibling, ev-device-sim) | MQTT `charger/${DEVICE}/#` topics, trigger, delivery ledger, atomA/B/C slugs | its lifecycle *is* the Delivery module — but under private names; flash-constrained (92–95%, USB-only reflash ⇒ changes are expensive and must batch) |
| pecan (mint + console) | melt budget, delivery ledger, STOPPED receipt, locked refund quote | the deposit pattern *is* the Meter module + settlement — its events are the terminal/progress reports under private names |
| evmap providers | per-provider schemas (status, tariffs, pay-and-charge) | the read-only map layer; the start-action surface wants grant vocabulary |
| phoneautomation SDKs | provider-native fields (zone, plate, EVSE, quote, receipt) | the external-segment reference adapters (poll-as-tick, receipt-as-truth) |
| silent.energy frontend | pay-and-charge UX | the wallet-side surface for the same instruments |

None of this is wrong code — it is the same machine described in four
dialects. Alignment is a translation problem before it is a code problem.

## Principles

1. **Terminology before code.** A glossary costs a day and unblocks every
   later step; renaming code before agreeing names churns.
2. **Simulation before hardware.** `ev-device-sim` becomes the first
   conforming Delivery module; sticks follow when a reflash is warranted
   anyway (they are flash-constrained and USB-flashed — batch changes).
3. **Traits before protocol.** Validate the module split as Rust traits in
   this repo; only propose wire messages (`Grant`, `TerminalReport`,
   `StatusQuery`) once the trait shape survives a second consumer.
4. **Additive renames, parity tests as guardrails.** The MQTT byte-parity
   suite is the safety net for any firmware vocabulary shift; new names
   arrive as aliases/payload-schema versions, old names deprecate slowly.
5. **Graduation path.** Design notes that survive experiments get PR'd into
   upstream `docs/design/core/`; this fork rebases on upstream regularly.

## Phases

### P0 — The Rosetta glossary (days, zero code)
One canonical table, versioned in this repo, mapping every concept across
all dialects. Seed rows:

| Canonical (this series) | tollgate-rs | firmware | pecan | provider adapters |
|---|---|---|---|---|
| grant (caps+lease) | session/channel open | trigger w/ budget | melt budget + slider start | start request + prestart quote |
| Delivery module | (implicit: forwarding plane) | charger relay/driver | daemon delivery ledger | bridge session driver |
| Meter module | node metering | — | mint-side accounting | quote/fee engine |
| PROGRESS report | MeteringReport | meter ticks | ledger accrual | session poll (kwh/time) |
| terminal report | channel close/settle | STOPPED receipt | STOPPED receipt | stop receipt (final fee) |
| lease expiry | channel exhaustion/rollover | connect timeout | quote expiry / refund window | plug/arrival timeout |
| refund (change) | — (channel model) | reversal command | locked refund quote | n/a (fiat settle) |

Rule: when any repo adds a concept, the glossary row lands in the same PR.

### P1 — Trait extraction here (the real experiment)
Implement in this fork: `DeliveryAdapter` (`grant() -> handle`, `status()`,
`revoke()`) + report types (`Progress{units, seconds, ts}`, terminal
variants with receipts) + a `MeterAgent` that consumes them against a
money-capped budget. Two consumers to validate the shape:
1. `ev-device-sim` wired behind the trait (host-side, cheap),
2. a network-forwarding adapter (wraps existing tollgate-net logic).

Conformance suite: grant → progress → each terminal outcome → lease expiry
→ crash-recovery reconciliation at last journalled reading.

### P2 — Firmware alignment (batched, flash-aware)
- Payload schema versioning on existing topics (not new topics): add
  canonical-named fields alongside legacy ones; log/state names adopt the
  glossary where free.
- A one-page mapping table in the firmware repo pointing at the glossary.
- **No reflash just for names.** Conformance-relevant *behavior* (explicit
  lease reporting, terminal receipts with meter totals) is the only thing
  worth a flash cycle — and it can wait for the next feature that forces
  USB anyway.
- Later: `tollgate-lab` gains a conformance tier — plug a stick in, run the
  P1 suite against real hardware.

### P3 — pecan event alignment (settlement)
Refactor pecan's charger flow to *emit* the canonical report events
(delivery ledger entries become typed Progress/Terminal reports) while the
mint side stays untouched. Result: pecan's deposit pattern is formally an
instance of the Meter module, and its STOPPED-receipt-vs-ledger refund
validation becomes the reference implementation of lease-crash
reconciliation.

### P4 — External-segment reference adapters
Generalize the phoneautomation SDKs into provider-neutral examples of
poll-as-tick DeliveryAdapters (quote sizing, receipt settle, idempotent
nonce↔session mapping). They become fixtures/examples for P1's conformance
suite — proving the traits work for both owned and third-party meters.

### P5 — evmap surface
The start-action UI adopts grant vocabulary (budget shown as money-cap +
time-cap; "reserve" language for deposits; refunds as change). Optional
instrument-tier features (tickets/seat-like entitlements) design against
the ecash-as-futures spectrum rather than ad hoc.

## Queued research questions (small, each an experiment note)

1. **Lease-crash reconciliation** — journal granularity vs refund latency;
   deliberate-kill harness (P1 suite covers it).
2. **Timestamped progress** — minimum report shape that lets any tariff be
   integrated after the fact (non-fungible-minutes implication).
3. **Exchange oracle** — prototype the check-in pattern: consume broad
   token, mint narrow entitlement (ecash-as-futures next step).
4. **Supply-capped keysets** — box-office reconciliation for class-event
   instruments (N seats ⇒ ≤N outstanding).
5. **Renewal semantics on external segments** — rate limits vs
   window-aligned re-grants (spot energy only, never parking).

## First moves (this week, cheap)

1. Write P0 glossary (half a day) and link it from every repo's README.
2. Stub the P1 traits + report enums in this fork with the sim adapter
   behind them; run the grant→terminal state machine as pure tests.
3. Add the glossary mapping table to atom-charger-display docs (no firmware
   change).
4. Rebase this fork on upstream to see what moved.

# Meter/Delivery modularity: budgeted delivery authorizations

*Experiment note (2026-09-13). Context: recent work integrating ecash payment
flows with two real-world providers — public EV charging and paid parking —
plus the ESP32 virtual charger fleet. This note generalizes what those
integrations taught us and proposes a module split for `tollgate-core`. No
protocol change is proposed yet; this is the design space exploration this
repo exists for.*

---

## The starting observation

TollGate today presumes **you own the meter**. The metering model
([tollgate-metering.md](../core/tollgate-metering.md)) is two peers counting
what they deliver/receive and reconciling cumulative counters each interval.
That is the right model when the same node gates the resource and counts it.

But the integrations we just built are a different shape. With an external
provider (a charging operator, a parking authority):

- **You do not gate delivery.** You call an API that asks the provider to
  start; the provider's infrastructure meters everything.
- **You can only observe**: poll a session endpoint for accrued energy/time,
  or receive a final receipt.
- **The provider demands a payment instrument up front** (a recurring-capable
  card), and pre-validates it against the maximum session horizon before it
  will start at all.
- **Final cost is computed at stop** — an open-ended amount that can only be
  bounded, not known, at start time.

Same economic contract as TollGate (pay for delivery), completely different
meter ownership. Rather than treating that as a special case, we think it
points at a generalization worth having in the core.

## "Melt into anything": time and units as budget dimensions

The deposit pattern we run on the charger fleet today — melt a budget,
deliver live, stop, settle actuals, refund the un-melted remainder —
generalizes cleanly to any good with an open-ended final price. Two budget
dimensions cover everything we have touched so far:

| Resource | Units dimension | Time dimension |
|---|---|---|
| Network access (tollgate-net) | bytes | seconds of session |
| EV charging | watt-hours | charging-session minutes / connect-time lease |
| Paid parking | — (pure time) | minutes, with tariff windows |
| Anything metered else (fluids, compute) | liters, cycles… | lease/lease-back windows |

A budget is therefore `{max_units, max_seconds}` — **whichever cap hits
first ends delivery**, plus a price vector to convert ticks into money. A
zero-price window (free nights, free tiers) is just a budget with a zero
price vector: keep the machinery uniform, skip the melt.

## The ESP32 virtual charger

The fleet that rehearsed all of this: ESP32-class devices acting as chargers
(Rust + Slint UI on stick-class boards, an MQTT byte-parity firmware family,
plus a simulator serving the same wire format). A charger is deliberately a
*commercial charger in miniature*: it has a display, a start/stop control
path, a meter (real or simulated), and it speaks the same lifecycle a public
charging point speaks. In TollGate terms the mapping we validated end to end:

| Commercial charger concept | Our ESP32 charger | TollGate concept |
|---|---|---|
| RemoteStartTransaction | MQTT trigger after melt | delivery authorization |
| meterStop − meterStart | local meter ticks to the delivery ledger | metered units |
| settlement reversal | locked refund quote vs the delivery ledger | change |
| connect-timeout (no plug) | session lease expiry, zero delivered | full refund path |

The virtual charger is the reference **Delivery module** (below): cheap
hardware, untrusted-by-default, capable of exactly what its grant allows and
nothing more.

## Proposed module split

Separate the **economic agent** from the **resource agent**:

```
Meter module (trust-holding)               Delivery module (resource agent)
───────────────────────────                ───────────────────────────────
holds payment / deposit        ── grant ─▶ receive AUTH {
consumes reports                          }   nonce, caps {units, seconds},
settles: burn actual A                   }   revocation handle, lease terms
refunds: B − A as change
                                             deliver
                               ◀─ reports ── terminal:
                                               COMPLETED(units, seconds)
                                               CAP_UNITS | CAP_SECONDS
                                               FAILED(err, delivered_so_far)
                                               REVOKED(delivered_so_far)
                                             progress: PROGRESS(units, seconds)
                                             queries:   STATUS() → snapshot
```

### Design rules the integrations taught us

1. **The grant is a capability, not a configuration.** Delivery modules live
   on constrained or third-party-controlled hardware. `{nonce, caps,
   revocation}` should be an unforgeable, scoped reference: a compromised
   delivery module can over-deliver at most up to its caps, never re-grant.
2. **Every authorization carries a lease.** Expiry-if-not-exercised is what
   makes crash recovery self-healing: no terminal report → lease lapses →
   settle at the last journalled reading. The charging world's connect
   timeout (start granted, no plug arrives, session auto-expires at zero
   delivered) is exactly this lease, observed in the wild.
3. **Push and pull metering are the same interface.** The ESP32 charger
   *pushes* meter ticks (MQTT callback); an external provider can only be
   *polled* (poll the session endpoint as the tick source). Both are drivers
   behind one trait; the core should not care which.
4. **The terminal report is the settlement receipt.** For local delivery it
   is our own signed ledger entry; for external providers it is the
   provider's own final response (their stop-receipt with the final fee and
   energy total). Settlement burns exactly what the receipt says — the
   receipt is the evidence, not our estimate of it.
5. **Quote before commit.** Providers expose a price-preview (an exact
   pre-commit quote for an arbitrary window). That call is the deposit
   estimator: budget B = quote(max window). It is also the free-tier guard:
   if the quote is zero, no melt is required.

### Failure taxonomy (all observed in practice)

| Outcome | Provider-side cause | Settlement |
|---|---|---|
| `FAILED` before delivery | payment instrument rejected at start | full refund |
| lease expiry, zero delivered | authorized but never exercised (no plug / no arrival) | full refund |
| `CAP_UNITS` / `CAP_SECONDS` | budget exhausted | settle quote-exact |
| `COMPLETED` early (stop command) | user/provider stopped before caps | settle receipt-actual, refund remainder |
| delivery-module crash | bridge/power loss mid-session | lease lapse → settle at last journalled reading |

## Tradeoffs when integrating external providers

The hard constraints, and what each costs:

- **Up-front deposits.** A provider that pre-authorizes a maximum horizon
  forces the bridge to front real float and to size budgets conservatively:
  larger deposits mean more stranded float and longer refund latency;
  smaller deposits mean renewal churn (re-grant loops near the cap). The
  lease/rollover mechanics from the payment-channel design map directly.
- **Instrument validity horizons.** Providers reject starts when the payment
  instrument expires before the *maximum* session end, not the requested
  one. A bridge must track instrument expiry as a first-class constraint on
  which grants it can issue at all.
- **Partial delivery is the normal case.** Stop-early is a feature (users
  stop when done); every session must be expected to settle below budget.
  Protocols that only handle all-or-nothing payment are unusable here —
  the change/refund path is not an edge case, it is the main path.
- **Poll cadence vs channel cadence.** Spilman-style streaming updates want
  frequent local ticks; external providers meter at their own granularity
  and are rate-limited. Consequence: the payment channel terminates at the
  trust-holding module (the bridge or our own charger's relay), and the
  segment below it settles against receipts, not streamed signatures. Two
  trust domains, one contract.
- **Idempotency.** Provider session ids are externally issued; retries after
  ambiguous failures must map one grant to at most one provider session
  (persist the nonce ↔ session-id mapping before starting).

## What this suggests for the core

Additive, not a rewrite: keep the current peer metering model as the
local-meter specialization, and introduce the split as traits — a
`DeliveryAdapter` (start-with-budget → handle; status; stop) and the meter
logic remaining in core. The existing cumulative-counter reconciliation and
transit-loss rules continue to apply on the local segment. New message
families (Grant / TerminalReport / StatusQuery) would be a protocol-level
follow-up if the trait shape survives experimentation.

## Next experiments

1. Refactor the ESP32 virtual charger behind the proposed `DeliveryAdapter`
   trait (it already implements the semantics; make the boundary explicit).
2. Bridge experiment: a meter module that fronts a delivery adapter driven
   by an external provider (charging or parking), with lease-expiry
   reconciliation under deliberate crashes.
3. Budget-dimension generality: run network access, charging, and parking
   through the same grant/report machinery with only price vectors
   differing.

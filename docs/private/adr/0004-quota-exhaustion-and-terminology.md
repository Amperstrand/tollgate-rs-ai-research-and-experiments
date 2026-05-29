# ADR-0004: Quota Exhaustion Handling, Adaptive Check-Ins, and Terminology

- **Status**: Accepted
- **Date**: 2026-05-07
- **Deciders**: Project owner, Sisyphus
- **Related**: #21 (negative balance bug, RFC 4006 analysis), #22 (chunk-based grants discussion)

## Context

During the M2 CDK integration test, the buyer's bootstrap balance went deeply negative (-53,000 scaled = -53 sat) across three metering intervals without the session being suspended. Investigation revealed:

1. **Bug**: `BootstrapSession::process_interval` deducts cost then checks `balance_scaled <= 0` — allowing overdraft when interval cost exceeds remaining balance
2. **Missing feature**: No pre-emptive mechanism to warn the buyer before exhaustion, no adaptive check-in intervals, no configurable leeway
3. **Terminology gap**: Balance tracking uses internal monetary terms but the protocol exposes no balance information to the buyer

Research into RFC 4006/8506 (Diameter Credit-Control Application), TollGate v1 (Go), fuel dispensers, API rate limiting, mobile data caps, and OCPP (EV charging) revealed consistent patterns across prepaid metering systems.

## Decision

### 1. No Dedicated Warning Message

We will NOT add a `BalanceWarning` message type to the protocol. Instead, we enrich the existing MeteringReport response with balance metadata — the same pattern used by RFC 4006 (`Final-Unit-Indication` on CCA), GitHub/Stripe (rate-limit headers on every response), and mobile data caps (throttle, don't warn).

**Rationale**: Every mature prepaid system puts balance metadata on regular responses, not in separate messages. This avoids protocol complexity (new encoding, state machine handling, lost/duplicate/out-of-order edge cases) while achieving the same goal.

### 2. Protocol Response: `remaining_quota` Terminology

The MeteringReport response will include a `remaining_quota` field (scaled i128) representing the buyer's remaining prepaid amount. Internal code continues using `balance_scaled`.

**Rationale**: "Remaining quota" is RFC 4006-aligned (closest to `Granted-Service-Unit`) and emphasizes the resource being metered rather than the monetary aspect. The buyer's internal balance IS the remaining service budget.

### 3. Adaptive Check-In Interval via `next_checkin_ms`

The MeteringReport response will include `next_checkin_ms` — the maximum time before the buyer MUST send the next MeteringReport. Computed as:

```
next_checkin_ms = remaining_quota / max_spend_rate
```

Where `max_spend_rate` is derived from pricing and the pipe's maximum throughput. Subject to a configurable floor (`min_checkin_ms`, default 1000ms).

**Rationale**: This is RFC 4006's `Validity-Time` concept. The buyer knows exactly when to check in next. As the quota decreases, the interval naturally shortens. This prevents the "burst near the end" problem — if the buyer has 50 MB remaining and the pipe max is 10 MB/s, they check in every 5 seconds.

### 4. Three Exhaustion Actions (Configurable)

When the buyer's quota approaches zero, the seller can take one of three actions (mirroring RFC 4006's `Final-Unit-Action`):

| Action | RFC 4006 Equivalent | Behavior |
|--------|-------------------|----------|
| **Terminate** | TERMINATE (0) | Hard cutoff. Suspend access immediately when `remaining_quota <= 0`. Default. |
| **Restrict** | RESTRICT_ACCESS (2) | Throttle bandwidth to a configured rate. Buyer can still use service but at reduced speed. Goodwill + gives time to top up. |
| **Allow** | No RFC equivalent | Deliver beyond the paid amount up to a configurable leeway (`leeway_percent` or `leeway_units_scaled`). Seller eats the overage cost. |

Configuration:

```yaml
exhaustion:
  action: Terminate           # Terminate | Restrict | Allow
  leeway_percent: 0           # Allow: deliver N% extra beyond paid amount (default: 0)
  leeway_units_scaled: 0      # Allow: deliver N extra scaled units beyond zero (default: 0)
  restrict_rate_bps: 0        # Restrict: throttle to N bits per second (default: 0 = no throttle)
  min_checkin_ms: 1000        # Floor for next_checkin_ms (default: 1000)
```

### 5. `is_final` Flag on MeteringReport Response

When the seller detects that the current interval's cost will consume the last of the buyer's quota, the response includes `is_final: true`. This is RFC 4006's `Final-Unit-Indication` — the seller tells the buyer "this is your last interval at full service."

### 6. Seller's Right to Disconnect at Any Time

Already supported by the existing `Disconnect` message. Explicitly documented: the seller may terminate a session at any time for any reason, including before balance exhaustion (e.g., capacity needed for a higher-paying peer) or allowing a brief overdraft as a courtesy.

### 7. Terminology Alignment with RFC 4006

| TollGate Concept | RFC 4006 Equivalent | Action |
|-----------------|-------------------|--------|
| MeteringReport response balance field | `Granted-Service-Unit` | Adopt `remaining_quota` name |
| Exhaustion actions | `Final-Unit-Action` | Adopt Terminate/Restrict/Allow |
| `is_final` flag | `Final-Unit-Indication` | Adopt the flag |
| Adaptive check-in interval | `Validity-Time` | Use `next_checkin_ms` (more precise than "validity") |
| `process_interval` result | Intermediate interrogation | Keep `Exhausted` (clearer than RFC's implicit handling) |
| `Suspended` access level | No direct equivalent | Keep (self-documenting) |
| MeteringReport fields | `Used-Service-Unit` | Keep `elapsed_ms`, `delivered`, `received` (more concrete than RFC's abstract CC-Time) |

### 8. NOT Adopted from RFC 4006

| RFC 4006 Concept | Why Not |
|-----------------|---------|
| Credit-Control-Server / OCS | TollGate is P2P, no central credit authority |
| `CC-Request-Type` (INITIAL/UPDATE/TERMINATION) | TollGate's separate message types are more explicit |
| `Requested-Service-Unit` | Buyer sends payment (token), not quota requests |
| `Multiple-Services-Credit-Control` | TollGate uses products/adapters |
| `Check-Balance-Result` | No balance-check round trip needed |
| `Rating-Group` | No shared-credit grouping |
| `Redirect-Server` | No captive portal concept |
| Chunk-based quota grants | Deferred to #22 — needs separate discussion |

## Consequences

- **Positive**: Prevents negative balance (the immediate bug)
- **Positive**: Buyer gets proactive information about remaining quota and next check-in
- **Positive**: Three exhaustion actions cover Terminate (strict), Restrict (graceful), Allow (generous) — covers gas pump, bandwidth throttle, and overdelivery scenarios
- **Positive**: Terminology aligns with RFC 4006 where appropriate, making the protocol familiar to anyone who's worked with prepaid systems
- **Positive**: No new message types — keeps the protocol simple
- **Negative**: MeteringReport response gets larger (3 new fields: `remaining_quota`, `next_checkin_ms`, `is_final`)
- **Negative**: Provider must compute `max_spend_rate` — requires knowing the pipe's maximum throughput for data-based pricing
- **Negative**: Throttle action (Restrict) requires OS-specific network adapter integration in `tollgate-net`
- **Risk**: If buyers don't respect `next_checkin_ms`, the provider must handle late reports gracefully

## Implementation Plan

1. Fix `process_interval` to pre-check against max possible cost before deducting
2. Add `remaining_quota`, `next_checkin_ms`, `is_final` to MeteringReport response struct
3. Implement `next_checkin_ms` computation from `remaining_quota / max_spend_rate`
4. Add `min_checkin_ms` floor (default 1000ms)
5. Implement configurable exhaustion actions (Terminate/Restrict/Allow)
6. Add `leeway_percent` / `leeway_units_scaled` configuration
7. Update `tollgate-bootstrap.md` with quota/terminology semantics
8. Update `tollgate-configuration.md` with new config options

## References

- #21 — Negative balance bug, full RFC 4006 analysis, implementation lessons
- #22 — Chunk-based grants discussion (deferred)
- RFC 8506 (2019) — Diameter Credit-Control Application (obsoletes RFC 4006)
- RFC 4006 §5.6 — Graceful Service Termination
- RFC 4006 §8.17 — Granted-Service-Unit AVP
- RFC 4006 §8.33 — Validity-Time AVP
- RFC 4006 §8.34–8.35 — Final-Unit-Indication / Final-Unit-Action AVPs
- jDiameter `ChargingServerSimulator` — `min(requested, balance)` quota grant pattern
- Magma mock OCS — configurable final-unit actions (Terminate/Redirect/RestrictAccess)
- FreeRADIUS `sqlcounter` — adaptive timeout extension pattern
- Gilbarco US Patent 5,868,179 — fuel dispenser flow rate ramp-down
- TollGate v1 Go: `setupThresholdTimers()`, `HandleUpcomingRenewal()` callback

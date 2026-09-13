# Non-fungible minutes: tariff granularity in budgeted delivery

*Experiment note (2026-09-13), companion to
[meter-delivery-modularity.md](meter-delivery-modularity.md). Trigger: if a
budget is "N minutes", what is a minute worth when the price per minute
changes under you?*

---

## The dilemma

A budgeted grant `{max_units, max_seconds}` presumes units are fungible:
that minute 30 costs what minute 1 cost. Real tariffs break this in three
independent dimensions:

1. **Wall-clock windows** — peak vs off-peak, day/night, weekday/weekend.
   Start off-peak, end in peak: the session's minutes live in different
   price worlds.
2. **Cumulative duration** — stepped tariffs ("first 6 hours at rate A,
   thereafter rate B"): the *same wall-clock minute* costs double once you
   cross a duration threshold.
3. **Cumulative units with time-varying unit price** — spot-priced energy
   where the per-kWh price changes hourly (and can combine with a
   per-minute occupancy price on top).

So the price of the next unit is a function `p(t_elapsed, t_wallclock,
u_cumulative) → money`. Raw "minutes" or "kWh" totals are not money, and the
mapping from a budget declared at t₀ to actual cost at settle is only
bounded, never known.

Two further properties observed with real providers:

- **Quote APIs integrate the schedule for you.** Ask for any window (even
  open-ended, auto-extended) and the provider returns the exact blended
  price for that window — computed with their tariff, including stepped and
  windowed rules.
- **Final receipts, not tick sums, are settlement truth.** The provider's
  stop-receipt carries the fee they will charge, computed their way. Our
  tick integration is only an estimator for budget guarding.

## Options

### A. Money as a third cap: `{max_seconds, max_units, max_cost}` ✅ recommended

The grant gains a money dimension; **whichever cap hits first ends
delivery**. The trust-holding module converts ticks to cost using the rate
schedule; the delivery module stays price-blind (capability minimalism —
untrusted hardware needs no tariff knowledge). Deposit = `max_cost`, refund
the unburned remainder at settle.

- Handles every tariff shape (windowed, stepped, spot) with one mechanism.
- Open-ended sessions: `max_cost` must be sized for the **worst contiguous
  window** within the lease (peak rate × remaining horizon) — the
  overpay-and-refund cost of not promising an end time.
- Cost: stranded float proportional to pessimism; refund latency.

### B. Rolling renewal — grants per tariff window

Issue short grants aligned to windows; renew at the boundary at then-current
rates (the payment-channel rollover pattern applied to time). Minimal float,
tracks spot prices.

- Fatal annoyance: a failed renewal at a peak boundary **kills a session
  mid-delivery** — the car is already parked, the session is already
  running. You cannot repossess delivered parking.
- Renewal churn vs provider rate limits.
- Sensible for energy spot exposure; poor fit for parking.

### C. Denominated ecash — "peak-minute" / "off-peak-minute" keysets ❌ considered, rejected

Issue keysets per tariff class; the delivery mechanism melts the matching
note type per tick. Bearer instruments whose face value *is* the tariff
unit; no trust in the meter's arithmetic.

Rejected because:

1. **Keyset explosion**: provider × window class × every tariff revision.
2. **Change-making becomes FX**: hold 100 off-peak-minutes, need 30
   peak-minutes → the mint (or someone) must make a market on a tariff it
   does not control, at an exchange rate that changes with the schedule.
   Cashu swaps make change within a unit, not across units.
3. **Tariff revisions strand notes**: provider reprices → outstanding
   keysets reference a dead price → who redeems them, at par?
4. It pushes tariff topology into every delivery module — the opposite of
   the capability-minimalism rule.

### D. The local segment is already solved: sign money, not units

Spilman channel updates carry **money deltas**. Per-tick price variation on
a segment you control is therefore free: metering counts units, pricing
converts at the current rate, the signature covers money. The dilemma only
exists where amounts are fixed at grant time (deposits/melts) and in
*declaring* budgets — which is exactly why the money cap (A) belongs in the
grant itself.

## Protocol implication for reports

If minutes are non-fungible, totals are not enough for cost accrual on the
local segment: **progress reports need timestamps** (or per-window buckets)
so the meter module can integrate any schedule, including ones that change
mid-session. Cumulative counters plus timestamps suffice — no per-window
accounting in the delivery module. On external segments this is moot: the
provider's receipt is the only pricing truth; our integration is a guard,
not a bill.

## Recommendation

1. Grants: `{max_seconds, max_units, max_cost, lease}` — three caps and a
   lease; money-denominated only.
2. Open-ended sessions deposit at worst-window rates; quote APIs (where
   available) size the deposit; receipts settle.
3. Spot-exposed energy may layer rolling renewal on top of the money cap;
   parking never needs it.
4. Denominated ecash: documented here so we never re-litigate it.

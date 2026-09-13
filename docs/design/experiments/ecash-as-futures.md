# Ecash as futures: the fungibility spectrum of deliverable goods

*Experiment note (2026-09-13), third in the series after
[meter-delivery-modularity.md](meter-delivery-modularity.md) and
[non-fungible-minutes.md](non-fungible-minutes.md). It generalizes both: the
budget/grant machinery is one point in a spectrum of what an ecash
instrument can promise.*

---

## The framing

A piece of ecash is a **futures contract for delivery**. The mint is the
issuer of a promise; the keyset (its unit) is the contract's underlying;
melting is exercising the contract; swapping is change-making *within* the
same underlying. What varies across everything we have built and sketched is
**how specific the promised good is** — a spectrum from pure commodity to
unique entitlement.

## The spectrum

| Fungibility | Instrument | Example | Cashu shape |
|---|---|---|---|
| Pure commodity | "one unit of X, any X" | a kilowatt-hour; a minute (rate caveats below) | keyset with unit `kwh` / `min` |
| Scoped commodity | "one unit of X *within scope S*" | gift card: any movie at a given cinema | keyset per scope (unit `cinema-X-movie`) |
| Class event | "one instance of X at time T" | ticket to a specific showing | keyset per showing; supply-capped |
| Unique entitlement | "this specific instance" | a specific seat at a specific showing; seat 12A on flight SK451 | serialized token / locked output bound to the instance |

Two structural observations that cut across the spectrum:

### 1. Exchange chains narrow entitlements

Flights show the canonical chain: hold a **flight-right** (fungible within
the flight), present it at check-in, receive a **seat-right** (unique,
assigned). The instrument is *transformed* by an exchange step — check-in is
an oracle that consumes a broader promise and materializes a narrower one.
The same shape appears everywhere once you look: a cinema showing ticket
becomes a seat when you pick one; a charging budget becomes a session bound
to one plug.

### 2. Fungible goods still deliver through non-fungible channels

Minutes and kilowatt-hours are fungible as *quantities*, but delivery is
always an entitlement: **"plate DP… may occupy zone 2300"**, **"EVSE 8e77…
may deliver energy"**. The fungible commodity rides inside a non-fungible,
scoped authorization — exactly the grant `{nonce, caps, lease}` from the
modularity note. In futures terms: the melt buys the commodity; the grant is
the delivery instrument that names *where* and *to what* it may be delivered.

Even the lease has a futures analog: an unexercised right that lapses is an
**option expiring worthless** — the connect-timeout window (authorized, no
plug arrives, expires at zero delivered) is precisely that.

## Mapping to Cashu primitives

- **Unit / keyset = the underlying.** Different units are different
  promises; this is the native "coloring" mechanism (no tainting within a
  keyset needed — issue a keyset per promise).
- **Spending conditions (P2PK etc.) bind *who*** may exercise; **the unit
  binds *what*** is delivered. Scope lives in the unit, not the signature.
- **Swaps** make change within one underlying. **Cross-underlying exchange**
  (minutes → seat; off-peak-minutes → peak-minutes) is *not* a swap — it
  requires a market or an oracle-mediated transformation: the check-in desk,
  the bridge, the mint acting as dealer. This is why denominated tariff
  keysets failed (non-fungible-minutes.md): they created undeclared FX.
- **Supply-capped keysets** (a showing has N seats; a flight has M) make
  class-event instruments honest: the mint must not issue more promises than
  instances exist. That reconciliation is exactly what a box office does.

## When to use which

- **Fungible + money-capped grants** (modularity note): open-ended metered
  delivery — parking, charging, bandwidth. Deposit in money, entitlement in
  the grant, receipts settle. Don't color the money.
- **Scoped instruments**: when the good is naturally countable and
  supply-capped and redemption is atomic — tickets, seats, vouchers. Color
  via keyset/unit; let an exchange step narrow them.
- **Transformation chains**: model check-in-like steps as explicit
  oracle exchanges (consume broad token → mint narrow token), so every
  intermediate state is bearer-ownable and audit-friendly.

## Why this matters for TollGate

TollGate's streaming model is the *commodity end* of the spectrum (metered
units, money-signed updates). The entitlement end — tickets, seats,
reservations — is reachable with the same mint infrastructure but different
instruments, and the middle (fungible units delivered via scoped
entitlements) is exactly the grant/report contract. Naming the spectrum lets
us pick the right instrument per integration instead of forcing everything
into metered delivery — and warns us where FX-like hazards appear the moment
two "colors" must be exchanged without an oracle.

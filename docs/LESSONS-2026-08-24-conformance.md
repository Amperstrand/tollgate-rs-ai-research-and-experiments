# Lessons — conformance testing against the reference implementation (2026-08-24)

Context: pointing this fork's reference Rust client at the silent.energy
TollGate façade (production, real signet ecash) for T2 conformance.

## 1. Your own test clients inherit your own blind spots

The façade had shipped with a **32-byte self-pubkey in Announce** (a raw
SHA-256 digest). Both of our test clients accepted it — the JS conformance
client decodes CBOR generically, the vendored python client doesn't enforce
field lengths. The reference `Announce` decoder types the field
`ByteArray<33>` (compressed secp256k1) and **dropped the whole message**.
Symptom was misleading ("peer did not return an Announce") while the
payment underneath had actually succeeded.

**Rule:** conformance-test against the *strictest* decoder you can find —
the reference implementation — not mirrors of your own reading of the CDDL.
A PASS from your own client is a smoke test, not conformance.

## 2. The reference `pay` assumed single-product peers

`client::pay` sent Announce + BootstrapToken with **no Accept**. Against a
catalog gateway (13 products) that requires product selection, the token
arrives with no charger chosen. Fixed in this fork: `pay` now probes the
sheet first and, for multi-product peers, sends an `Accept` for the first
product + the mint option matching `--mint` (fallback: first option).

## 3. Real-token funding path

The reference client fabricates test tokens (fake-mint topologies — "the
fake mint ignores C's value"). For production gateways that verify with a
real mint, `pay --token <cashu…>` now pays a pre-minted wallet token.
Funding flow used: signut bolt11 quote → paid via our clnrest rune →
cashu-ts mints proofs → token handed to the Rust client.

## 4. Scripted edits must assert

A python string-replace no-oped on a needle mismatch (`{e:?}", e);` vs
`{e:?}")`), and the stale call site survived until `docker build` failed
with E0061. The compile gate caught it — but one build cycle was wasted.
**Rule:** every scripted replace asserts the needle count before/after;
the repo now carries `scripts/hooks/pre-commit` (cargo check gate) so the
class dies at commit time on toolchain-equipped machines, and warns loudly
otherwise.

## Result

`PAID peer=02b88cb… accepted=true` — reference client → façade → real
Cashu verification → charger started, plus `PRICESHEET products=13
mints=3 per_second=1000` decoded by their parser. Façade fixed to emit a
33-byte key (silent.energy ff734c15).

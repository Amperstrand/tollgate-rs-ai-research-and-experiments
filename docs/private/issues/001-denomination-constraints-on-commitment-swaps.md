# Issue #001: Denomination Constraints on Commitment Swaps

**Status**: Resolved
**Priority**: Medium
**Area**: Spilman channel demo, protocol accuracy
**Created**: 2026-05-10
**Resolved**: 2026-05-10

## Background

In our Spilman demo, a 100 sat channel is funded with proofs of denominations [64, 32, 4] (greedy binary decomposition). The user asks: how do you pay 5 sat from a channel funded with [64, 32, 4]? Don't you need a 1-denomination proof?

## Resolution

### Answer: No. Any payment amount is possible. Payments are signatures only.

The key insight from the canonical SatsAndSports ARCHITECTURE.md and the Cashu NUT-XX PR (#296):

**During the channel (open/funded state):** No proofs are created or split. Alice signs a **commitment swap** — a full specification of inputs (funding token) and outputs (deterministic split for any balance). This is just a Schnorr signature with SIG_ALL. No mint interaction.

**At settlement (close):** The mint's `/v1/swap` takes the funding proofs [64, 32, 4] as inputs and creates **fresh proofs** in whatever denominations are needed for the final split. The mint has keys for all denominations (1, 2, 4, 8, 16, 32, 64). So any integer amount is representable.

**Why [64, 32, 4] and not all powers of 2?** Binary decomposition is greedy — it uses the fewest proofs possible. 100 = 64 + 32 + 4 (three proofs). For 5 sat, it would be 4 + 1 (two proofs). For 7 sat, 4 + 2 + 1 (three proofs). Every integer has a unique binary representation. You don't need all denominations pre-created; the mint creates whatever denominations are needed at settlement.

### Canonical Source

From `SatsAndSports/cashu_spilman_channels/ARCHITECTURE.md`:

> "Alice authorizes balance updates by signing a Cashu **commitment swap**. This swap spends the funding token and creates **deterministic Stage 1 outputs** for both Charlie (his earned balance) and Alice (her remaining change).
> Alice signs the request using the **SIG_ALL** flag, ensuring the signature commits to the specific inputs and outputs."

The outputs are **deterministically derived** from the channel secret + balance amount. Both parties independently derive the same output denominations for any balance. No trust required — the SIG_ALL signature is atomic and mathematically binding.

### Our Demo's Simplification

Our demo signs `SHA256(channel_id || "|" || balance)` — just the balance number. The real cdk-spilman uses `sig_all_message_hash` which serializes the **full swap request** (inputs + outputs with specific blinded messages). This is a documented simplification in our README. The real implementation would specify exact output denominations for every balance update, making the signature commit to the full swap.

### Unilateral Close

For unilateral close: the receiver submits the pre-signed commitment swap to the mint. The swap specifies exact output denominations (deterministically derived), so any amount is settleable. The funding proof denominations don't constrain the output denominations — the mint only checks input total ≥ output total + fee.

## Actions Taken

- Updated educational text in demo to explain: "No proofs are created during payments — only signatures. The proofs only get split at settlement."
- Updated Step 2 text to explain why [64, 32, 4] and that binary decomposition works for any amount
- Updated Step 3/4/5 texts to clarify the distinction between signature-time and settlement-time

# How Spilman Channels Work

A walkthrough for the [live demo](https://amperstrand.github.io/tollgate-rs-ai-research-and-experiments/spilman-real/). Open that link, click "Run Full Lifecycle", and follow along below.

## The Cast

**Alice** is the buyer. She locks up ecash and pays incrementally. **Charlie** is the seller. He receives payments and settles at the end. **The mint** (testnut.cashu.exchange) is the Cashu ecash issuer. It holds Alice's locked tokens and performs the final split at settlement.

The demo has three columns: Alice panel (left), center panel with the channel state, and Charlie panel (right). Below that, Mint Requests shows every HTTP call to the mint. The Debug Panel at the bottom logs each operation.

## Step 1: Open Channel

Alice and Charlie need a shared secret. Neither trusts the other, so they can't just pick one and send it. Instead they do ECDH.

Both generate a secp256k1 private key locally. They exchange public keys. Each computes `compute_channel_secret(myPrivKey, theirPubKey)`, which does a domain-separated ECDH: derive the shared point on the curve, hash it with SHA-256, and return the result. Because ECDH is commutative, both get the same 32-byte channel secret without ever transmitting it.

Alice also fetches the mint's active keyset (which public keys the mint is currently using for signing, plus the fee rate). She assembles all channel parameters into a struct: mint URL, unit, capacity (100 sat), the public keys, timestamps, fee info. The channel ID is `SHA-256(serialized_params || channel_secret)`. This binds the channel identity to both parties' keys and the mint's keyset.

**What moves between Alice and Charlie:** Public keys (in clear). That's it. The channel secret never crosses the wire.

**What the mint sees:** Two GET requests: `/v1/keysets` and `/v1/keys/{keysetId}`. The mint has no idea a channel is being opened. It's just serving public key info.

**In the demo UI:** Both panels show their public key. The center panel shows "INIT" and "Not yet opened". The step dots highlight step 1.

## Step 2: Deposit (Fund)

Alice needs to lock 100 sat into the channel. She does this by minting ecash from the mint, but with a twist: the proofs are locked under a 2-of-2 multisig spending condition requiring both Alice and Charlie to cooperate.

Here's the sequence:

1. Alice calls `compute_funding_token_amount(100, keysetInfo, maxPerOutput=64)`. This returns the exact amount to mint, accounting for the mint's fee. Cashu mints charge a fee per input at settlement, so Alice mints slightly more than 100 to cover future costs. The result depends on the mint's `input_fee_ppk`.

2. Alice requests a Lightning mint quote from the mint (`POST /v1/mint/quote/bolt11`). testnut auto-pays the invoice. She polls until paid.

3. Alice creates deterministic blinded outputs using `create_funding_outputs`. This is where P2BK ("Proof-to-Blinding-Key") comes in. Instead of generating random blinding factors, Alice derives them deterministically from the channel secret:

   ```
   blinding_scalar = SHA256("Cashu_Spilman_P2BK_v1" || channel_secret || "{channel_id}|funding|{retry}")
   ```

   Retry increments from 0 until the scalar is a valid secp256k1 private key (between 0 and the curve order). The blinding factor is also deterministically linked to Alice and Charlie's public keys, creating a 2-of-2 P2PK spending condition on the resulting proofs.

   Cashu uses binary denominations, so 100 sat becomes proofs worth [64, 32, 4]. Each proof has its own blinded message sent to the mint.

4. Alice posts the blinded outputs to the mint (`POST /v1/mint/bolt11`). The mint signs each blinded message with its private key and returns blind signatures. The mint can't see what it's signing, only the amount and the blinded point.

5. Alice unblinds the signatures using `construct_proofs`. Each blind signature, combined with the original blinding factor and secret, becomes a spending proof. These proofs are the "funding token" for the channel.

Alice then gives Charlie the funding proofs and her private key (yes, her actual private key, needed for the cooperative close witness). In production, she'd sign close transactions on demand instead of sharing the key. The demo simplifies this.

**What moves:** Alice sends proofs and her private key to Charlie via direct function call (same page, no network).

**What the mint sees:** `POST /v1/mint/quote/bolt11` (create quote), `GET /v1/mint/quote/bolt11/{id}` (poll), `POST /v1/mint/bolt11` (mint). The mint sees Alice minting tokens. It doesn't know they're for a channel.

**In the demo UI:** The Mint Requests panel shows three entries. Alice's proof count goes to 3 (64 + 32 + 4 = 100). The Funding Lock section in the center fills with token bars showing [64, 32, 4]. The spending condition reads "(Alice + Charlie) OR (Alice after expiry)". Channel state changes to FUNDED.

## Step 3: Pay (First Payment, 10 sat)

Now things get interesting. Alice sends 10 sat to Charlie. But no ecash actually moves. No proofs change hands. The mint isn't contacted at all.

Instead, Alice constructs a "commitment swap": a full specification of how the funding token should be split. The swap says: take the funding proofs [64, 32, 4] as inputs, create outputs for 10 sat to Charlie and 90 sat back to Alice. The output addresses are derived deterministically from the channel secret, so both parties compute the same outputs independently.

Alice signs this swap specification with `spilman_channel_sender_create_signed_balance_update`. This function:

1. Constructs the full swap (inputs + outputs) with all amounts and blinded points
2. Serializes it into the SIG_ALL message format: `secret_0 || C_0 || ... || secret_n || C_n || amount_0 || B_0 || ... || amount_m || B_m`
3. Hashes it with SHA-256
4. Signs the hash with a Schnorr signature using Alice's private key tweaked with a channel-secret-derived scalar

The result is a single Schnorr signature that commits to every input and every output. Alice hands this signature to Charlie. Charlie stores it. That's the payment.

**Why this works:** The SIG_ALL signature binds Alice to this exact split. She can't later claim a different split was agreed. If she tries to submit an older swap to the mint, Charlie can prove it's outdated because he holds a newer signature for a higher balance. (The demo only implements cooperative close, so this dispute mechanism isn't exercised here.)

**What moves:** One Schnorr signature from Alice to Charlie. About 64 bytes. That's the entire payment.

**What the mint sees:** Nothing. No HTTP calls. Payments are purely off-chain.

**In the demo UI:** The center panel's split bar shifts: Charlie now owns 10% (10 sat), Alice owns 90% (90 sat). The Commitment Transaction card shows the signature details. The "SIG_ALL" badge means Alice signed all inputs and outputs atomically.

## Step 4: Meter (Second Payment, 20 sat)

Same mechanism as step 3. Alice constructs another commitment swap: this time 30 sat to Charlie (cumulative), 70 sat back to Alice. Signs with SIG_ALL. Hands the signature to Charlie.

The old 10-sat commitment is now superseded. Charlie discards it and keeps only the latest one. This is the core of streaming payment: each signature replaces the previous one, incrementally moving value from Alice to Charlie. No proof transfers, no mint interaction, no settlement delays.

**What moves:** One signature (again ~64 bytes).

**What the mint sees:** Still nothing.

**In the demo UI:** The split bar shifts again: Charlie 30%, Alice 70%. A new entry appears in the Commitment Transaction card. The previous commitment moves to "Superseded Commitments" below it. You can click the superseded one to see the old 10-sat split.

## Step 5: Close (Cooperative Settlement)

Charlie initiates close. He submits the latest commitment swap to the mint. This is where the proofs actually get split.

Charlie constructs the settlement swap:

1. **Inputs:** The original funding proofs [64, 32, 4]. Three inputs totaling 100 sat.

2. **Outputs for Charlie (30 sat):** Derived deterministically from the channel secret with context "receiver". Cashu uses binary denominations, so 30 = 16 + 8 + 4 + 2. Three proofs become four: [16, 8, 4, 2].

3. **Outputs for Alice (remaining):** Derived with context "sender". The remaining amount is `capacity - charlieBalance - mintFee`. The mint fee is `ceil(inputTotal * input_fee_ppk / 1000)`. The exact number depends on the mint's current fee rate. With 3 inputs and testnut's typical fee settings, Alice gets around 69 sat back (70 sat theoretical refund minus the fee).

4. **Witness:** Charlie needs both parties' signatures to spend the funding proofs (2-of-2 multisig). He computes two Schnorr signatures:

   - Alice's signature: `sign_with_tweaked_key(alicePrivKey, sigAllHash, senderTweak)` where `senderTweak = deriveBlindingScalar(channelSecret, channelId, "sender_stage1")`
   - Charlie's signature: `sign_with_tweaked_key(charliePrivKey, sigAllHash, receiverTweak)` where `receiverTweak = deriveBlindingScalar(channelSecret, channelId, "receiver_stage1")`

   The `sigAllHash` is computed over the full swap: all input secrets and C values, all output amounts and B_ values.

5. Charlie posts `POST /v1/swap` to the mint with all inputs, outputs, and the witness containing both signatures. The mint verifies the 2-of-2 multisig, checks the proofs haven't been spent, and returns fresh blind signatures for the new outputs.

6. Both sets of new proofs are unblinded via `construct_proofs`. Charlie gets his [16, 8, 4, 2]. Alice gets her refund proofs.

**What moves:** Charlie sends one HTTP request to the mint.

**What the mint sees:** `POST /v1/swap`. The mint sees three inputs being spent and new outputs being created. It verifies the spending condition (2-of-2 multisig witness) and issues fresh signatures.

**In the demo UI:** Both panels update with final proof counts. Charlie has 4 proofs totaling 30 sat. Alice has refund proofs. The Funding Lock shows "SETTLED". The Mint Requests panel gets a final `/v1/swap` entry.

## The Funding Lock

The funding proofs are locked under a 2-of-2 P2PK multisig. The spending condition is:

```
(Alice AND Charlie) OR (Alice after timeout)
```

Both parties must cooperate to spend the funding token during the channel's lifetime. If Charlie disappears, Alice can wait for the timeout (1 hour in the demo) and reclaim her full deposit unilaterally. This prevents Charlie from holding Alice's funds hostage.

The timeout close path isn't implemented in the demo, but the spending condition is real. The mint enforces it: any swap spending the funding proofs must include valid signatures from both parties, unless the channel has expired.

## Commitment Swaps

Each payment is a complete description of how to split the funding token, signed by Alice. Think of it as a check that says "if this channel settles right now, Charlie gets X sat and I get the rest."

The commitment includes every input (the three funding proofs) and every output (Charlie's share, Alice's share, all in binary denominations). Alice signs the whole thing with SIG_ALL. She can't dispute the split later because her signature commits to the exact amounts and addresses.

Only the latest commitment matters. When Alice sends a new payment, the old signature becomes worthless. Charlie keeps only the most recent one. This is why payments can stream without accumulating state: each one is a full replacement, not an incremental update.

## SIG_ALL

SIG_ALL means Alice signs all inputs and all outputs of the swap transaction atomically. The message she signs is:

```
secret_0 || C_0 || ... || secret_n || C_n || amount_0 || B_0 || ... || amount_m || B_m
```

Where secrets and C values come from the funding proofs (inputs), and amounts and B_ values come from the settlement outputs. This is hashed with SHA-256, then signed with Schnorr.

The "ALL" part is critical. In some signature schemes, you only sign inputs (so you can swap outputs later). SIG_ALL prevents this. Once Alice signs, the outputs are frozen. She can't claim she meant to send less to Charlie, because the output amounts are part of the signed message.

## Why No Proofs During Payment

This is the most counterintuitive part of Spilman channels. During steps 3 and 4, no ecash tokens change hands. No proofs are created, destroyed, or transferred. Alice doesn't send Charlie a 10-sat token.

Instead, Alice signs a promise: "if we settle now, here's exactly how the money splits." The signature is the payment. Proofs only get created at settlement (step 5), when the mint actually splits the funding token.

This has two big advantages. First, no mint interaction during payments. Alice and Charlie can make thousands of micropayments without the mint knowing or caring. Second, atomic settlement. When the channel closes, a single mint transaction handles the entire split. There's no intermediate state where some proofs went to Charlie but others didn't.

## Mint Fees

Cashu mints charge a fee per input when you swap or melt tokens. The fee is specified as `input_fee_ppk` (parts per thousand). If the fee is 1 ppk and you submit 4 inputs, the fee is `ceil(4 * 1000 / 1000)` = 4 ppk worth. Wait, that's not right. The formula is `ceil(inputTotal * input_fee_ppk / 1000)` where `inputTotal` is the satoshi sum of the inputs.

In the demo's close step: the funding token has 3 proofs (let's say totaling 100+ sat, depending on `compute_funding_token_amount`). The fee is `ceil(total * fee_ppk / 1000)`. This fee comes out of Alice's refund, not Charlie's payment. Charlie gets his full 30 sat. Alice gets `capacity - 30 - fee`.

The fee exists because the mint has to store each spent proof forever (to prevent double-spending). More inputs means more storage, so the mint charges per input.

## Try It Yourself

**Watch mint requests per step.** The Mint Requests panel at the bottom of the center column logs every HTTP call. Run the lifecycle step by step (use the individual Step buttons, not "Run Full Lifecycle"). Notice that steps 3 and 4 produce zero mint requests. The mint only sees the open/fund/close phases.

**Compare proof counts.** Alice starts with 3 funding proofs [64, 32, 4]. After close, Charlie has 4 proofs for 30 sat [16, 8, 4, 2] and Alice gets refund proofs. The proof counts change at close, not during payments.

**Use the custom payment slider.** After funding (step 2), the slider activates. Drag it to see different amounts. The Settlement Breakdown below it shows how the binary denominations would split. Try 1 sat: Charlie gets [1], and Alice gets a refund split across many small denominations. Try 50 sat: Charlie gets [32, 16, 2], and Alice's refund shrinks.

**Check the superseded commitments.** After two payments, look at the "Superseded Commitments" section in the center panel. The first payment's commitment is grayed out. Only the latest is active. If you could submit the old one to the mint (you can't in this demo), Charlie would get less money. That's why Charlie always keeps the latest and discards the rest.

**Look at the debug panel.** Expand it at the bottom. It shows the actual channel ID (truncated), proof counts at each step, and the final settlement amounts. The channel ID changes every run because it's derived from fresh ECDH keys.

## Architecture Notes

**Cooperative close only.** The demo only implements the path where both parties agree to settle. A full implementation would also support unilateral close (Charlie submits the latest commitment without Alice's cooperation) and timeout close (Alice reclaims after the channel expires). Both require additional witness construction logic.

**Both wallets in the same page.** Alice calls `charlie.acceptPayment()` directly. In production, Alice and Charlie are separate processes communicating over a network. The crypto is identical either way.

**No DLEQ verification.** When the mint returns blind signatures, it also returns a DLEQ (Discrete Logarithm Equality) proof that it signed with the correct key. The demo parses these but doesn't verify them. This means the demo trusts the mint isn't signing with a different key than it published. A production wallet should verify.

**Low-level WASM bindings.** The demo calls individual WASM functions (`compute_channel_secret`, `construct_proofs`, `sign_with_tweaked_key`, etc.) and wires them together in JavaScript. The SatsAndSports reference implementation uses `WasmSpilmanBridge` and `SpilmanClientBridge` classes that handle this orchestration internally. The demo's approach is more verbose but makes each step visible and auditable.

**crypto.js still handles some operations.** Key generation (`generatePrivateKey`, `getPublicKey`), denomination splitting (`getDenominationAmounts`), and close output construction (`createDeterministicOutput` for "receiver"/"sender" contexts) remain in JavaScript. Everything else goes through cdk-wasm, compiled from the same Rust crate used by the reference implementation.

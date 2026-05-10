// wallet.js — Alice (buyer/sender) and Charlie (seller/receiver) wallet objects

import * as crypto from "./crypto.js";
import * as mint from "./mint.js";
import {
  createChannel,
  transitionToFunded,
  applyPayment,
  transitionToClosing,
  transitionToClosed,
  STATUS,
} from "./channel.js";

/**
 * Create Alice's wallet (buyer/sender).
 * Generates ephemeral keys, manages channel as sender.
 */
export function createAliceWallet() {
  const privKey = crypto.generatePrivateKey();
  const pubKey = crypto.getPublicKey(privKey);
  const privKeyHex = crypto.bytesToHex(privKey);
  const pubKeyHex = crypto.bytesToHex(pubKey);

  return {
    role: "alice",
    privKeyHex,
    pubKeyHex,
    channel: null,
    proofs: [],

    /** Open a channel with Charlie */
    async openChannel(charliePubKeyHex, { capacitySat = 100, maxPerOutput = 64 } = {}) {
      // 1. Compute channel secret via ECDH
      const channelSecretHex = crypto.computeChannelSecret(this.privKeyHex, charliePubKeyHex);
      const channelSecret = crypto.hexToBytes(channelSecretHex);

      // 2. Fetch keyset info from mint
      const keysetsResp = await mint.getKeysets();
      const activeSat = keysetsResp.keysets.find(ks => ks.unit === "sat" && ks.active);
      if (!activeSat) throw new Error("No active sat keyset");

      const keysetId = activeSat.id;
      const inputFeePpk = activeSat.input_fee_ppk || 0;

      const keysResp = await mint.getKeys(keysetId);
      const keysetKeys = keysResp.keysets[0].keys;

      // 3. Compute channel parameters
      const now = Math.floor(Date.now() / 1000);
      const expiry = now + 3600;

      // 4. Derive channel ID
      const params = {
        mint: mint.MINT_URL,
        unit: "sat",
        capacity: capacitySat,
        fundingTokenAmount: capacitySat,
        keysetId,
        inputFeePpk,
        maximumAmount: maxPerOutput,
        setupTimestamp: now,
        senderPubkey: this.pubKeyHex,
        receiverPubkey: charliePubKeyHex,
        expiryTimestamp: expiry,
      };

      const channelId = crypto.getChannelId(params, channelSecretHex);

      // 5. Create channel state
      this.channel = createChannel({
        channelId,
        channelSecret: channelSecretHex,
        capacity: capacitySat,
        params,
      });

      // Store keyset info for later use
      this._channelSecret = channelSecret;
      this._keysetKeys = keysetKeys;
      this._keysetId = keysetId;
      this._inputFeePpk = inputFeePpk;

      return { channelId, channelSecret: channelSecretHex, params };
    },

    /** Fund the channel by minting tokens from testnut */
    async fundChannel() {
      if (!this.channel) throw new Error("No open channel");

      const capacitySat = this.channel.capacity;

      // 1. Create mint quote (testnut auto-pays Lightning)
      const quote = await mint.postMintQuoteBolt11(capacitySat + 1000);
      await mint.pollMintQuote(quote.quote);

      // 2. Create deterministic blinded messages for funding
      const channelId = this.channel.id;
      const channelSecret = this._channelSecret;
      const amounts = crypto.getDenominationAmounts(capacitySat, this.channel.params.maximumAmount);

      const outputs = [];
      const secretsWithBlinding = [];

      for (let i = 0; i < amounts.length; i++) {
        const output = crypto.createDeterministicOutput(
          channelSecret, channelId, "funding", amounts[i], i,
        );
        outputs.push({ amount: amounts[i], B_: output.B_, id: this._keysetId });
        secretsWithBlinding.push({
          secret: output.secret,
          blinding_factor: output.blindingFactor,
          amount: amounts[i],
        });
      }

      // 3. Mint proofs
      const mintResp = await mint.postMintBolt11({ quote: quote.quote, outputs });

      // 4. Construct proofs from blind signatures
      const proofs = crypto.constructProofs(
        mintResp.signatures,
        secretsWithBlinding,
        this._keysetId,
        this._keysetKeys,
      );

      this.proofs = proofs;
      transitionToFunded(this.channel, proofs);

      return proofs;
    },

    /** Create a signed payment to Charlie */
    createPayment(amountSat) {
      if (!this.channel || this.channel.status !== STATUS.FUNDED) {
        throw new Error("Channel not funded");
      }

      const signedUpdate = crypto.createSignedBalanceUpdate(
        this.channel.params,
        this.privKeyHex,
        this.channel.channelSecret,
        this.channel.id,
        this.channel.balanceToReceiver + amountSat,
      );

      applyPayment(this.channel, amountSat, signedUpdate);

      return signedUpdate;
    },

    /** Get current balance */
    getBalance() {
      return this.channel
        ? {
            capacity: this.channel.capacity,
            spent: this.channel.balanceToReceiver,
            remaining: this.channel.capacity - this.channel.balanceToReceiver,
            status: this.channel.status,
          }
        : { capacity: 0, spent: 0, remaining: 0, status: "NONE" };
    },
  };
}

/**
 * Create Charlie's wallet (seller/receiver).
 * Generates ephemeral keys, receives payments.
 */
export function createCharlieWallet() {
  const privKey = crypto.generatePrivateKey();
  const pubKey = crypto.getPublicKey(privKey);
  const privKeyHex = crypto.bytesToHex(privKey);
  const pubKeyHex = crypto.bytesToHex(pubKey);

  return {
    role: "charlie",
    privKeyHex,
    pubKeyHex,
    channel: null,
    proofs: [],

    /** Accept a channel from Alice */
    acceptChannel(alicePubKeyHex, channelParams) {
      // Compute same channel secret (ECDH from Charlie's perspective)
      const channelSecretHex = crypto.computeChannelSecret(this.privKeyHex, alicePubKeyHex);
      const channelSecret = crypto.hexToBytes(channelSecretHex);

      // Derive same channel ID
      const channelId = crypto.getChannelId(channelParams, channelSecretHex);

      this.channel = createChannel({
        channelId,
        channelSecret: channelSecretHex,
        capacity: channelParams.capacity,
        params: channelParams,
      });

      this._channelSecret = channelSecret;

      return { channelId, channelSecret: channelSecretHex };
    },

    /** Accept funding from Alice */
    acceptFunding(fundingProofs) {
      if (!this.channel) throw new Error("No channel");
      transitionToFunded(this.channel, fundingProofs);
    },

    /** Accept a payment from Alice */
    acceptPayment(deltaSat, signedUpdate) {
      if (!this.channel) throw new Error("No channel");
      applyPayment(this.channel, deltaSat, signedUpdate);
    },

    /** Cooperative close: swap proofs at the mint */
    async cooperativeClose() {
      if (!this.channel || this.channel.status !== STATUS.FUNDED) {
        throw new Error("Channel not funded");
      }

      transitionToClosing(this.channel);

      const balanceToCharlie = this.channel.balanceToReceiver;
      const balanceToAlice = this.channel.capacity - balanceToCharlie;
      const maxPerOutput = this.channel.params.maximumAmount;
      const channelId = this.channel.id;
      const channelSecret = this._channelSecret;

      // Create Charlie's outputs (receiver)
      const charlieAmounts = balanceToCharlie > 0
        ? crypto.getDenominationAmounts(balanceToCharlie, maxPerOutput)
        : [];
      const charlieOutputs = [];
      const charlieSecrets = [];
      for (let i = 0; i < charlieAmounts.length; i++) {
        const output = crypto.createDeterministicOutput(
          channelSecret, channelId, "receiver", charlieAmounts[i], i,
        );
        charlieOutputs.push({ amount: charlieAmounts[i], B_: output.B_, id: this.channel.params.keysetId });
        charlieSecrets.push({
          secret: output.secret,
          blinding_factor: output.blindingFactor,
          amount: charlieAmounts[i],
        });
      }

      // Create Alice's outputs (sender refund)
      const aliceAmounts = balanceToAlice > 0
        ? crypto.getDenominationAmounts(balanceToAlice, maxPerOutput)
        : [];
      const aliceOutputs = [];
      const aliceSecrets = [];
      for (let i = 0; i < aliceAmounts.length; i++) {
        const output = crypto.createDeterministicOutput(
          channelSecret, channelId, "sender", aliceAmounts[i], i,
        );
        aliceOutputs.push({ amount: aliceAmounts[i], B_: output.B_, id: this.channel.params.keysetId });
        aliceSecrets.push({
          secret: output.secret,
          blinding_factor: output.blindingFactor,
          amount: aliceAmounts[i],
        });
      }

      // Use funding proofs as inputs
      const inputs = this.channel.fundingProofs.map(p => ({
        amount: p.amount,
        id: p.id,
        secret: p.secret,
        C: p.C,
      }));

      // Swap at mint
      const swapResp = await mint.postSwap({
        inputs,
        outputs: [...charlieOutputs, ...aliceOutputs],
      });

      // Construct Charlie's proofs from swap response
      const keysetsResp = await mint.getKeysets();
      const activeSat = keysetsResp.keysets.find(ks => ks.unit === "sat" && ks.active);
      const keysResp = await mint.getKeys(activeSat.id);
      const keysetKeys = keysResp.keysets[0].keys;

      // Split swap signatures: first N for Charlie, rest for Alice
      const charlieSigs = swapResp.signatures.slice(0, charlieOutputs.length);
      const aliceSigs = swapResp.signatures.slice(charlieOutputs.length);

      const charlieProofs = crypto.constructProofs(
        charlieSigs, charlieSecrets, this.channel.params.keysetId, keysetKeys,
      );
      const aliceProofs = aliceSigs.length > 0
        ? crypto.constructProofs(
            aliceSigs, aliceSecrets, this.channel.params.keysetId, keysetKeys,
          )
        : [];

      this.proofs = charlieProofs;

      transitionToClosed(this.channel, charlieProofs, aliceProofs);

      return {
        charlieProofs,
        aliceRefundProofs: aliceProofs,
        charlieTotal: charlieProofs.reduce((s, p) => s + p.amount, 0),
        aliceTotal: aliceProofs.reduce((s, p) => s + p.amount, 0),
      };
    },

    /** Get current balance */
    getBalance() {
      return this.channel
        ? {
            capacity: this.channel.capacity,
            received: this.channel.balanceToReceiver,
            status: this.channel.status,
          }
        : { capacity: 0, received: 0, status: "NONE" };
    },
  };
}

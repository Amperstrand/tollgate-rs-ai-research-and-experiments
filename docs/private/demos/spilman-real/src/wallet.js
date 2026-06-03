// wallet.js — Alice (sender) and Charlie (receiver) wallet objects
// NUT-XX naming: Alice=sender, Charlie=receiver, Bob=mint (https://github.com/cashubtc/nuts/pull/296)
//
// Wave C: Crypto operations delegated to cdk-wasm (WASM compiled from the same
// Rust crate that generates our test vectors). crypto.js kept for:
//   - generatePrivateKey / getPublicKey (not in WASM)
//   - getDenominationAmounts (not in WASM)
//   - createDeterministicOutput (cooperative close "receiver"/"sender" contexts)

import * as crypto from "./crypto.js";
import * as mint from "./mint.js";
import { toParamsJson, toKeysetInfoJson, wasm, deriveBlindingScalar, computeSigAllMessage, sha256Hex } from "./cdk-wasm-adapter.js";
import {
  createChannel,
  transitionToFunded,
  applyPayment,
  transitionToClosing,
  transitionToClosed,
  STATUS,
} from "./channel.js";

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

    async openChannel(charliePubKeyHex, { capacitySat = 100, maxPerOutput = 64 } = {}) {
      const channelSecretHex = wasm().compute_channel_secret(this.privKeyHex, charliePubKeyHex);

      const keysetsResp = await mint.getKeysets();
      const activeSat = keysetsResp.keysets.find(ks => ks.unit === "sat" && ks.active);
      if (!activeSat) throw new Error("No active sat keyset");

      const keysetId = activeSat.id;
      const inputFeePpk = activeSat.input_fee_ppk || 0;

      const keysResp = await mint.getKeys(keysetId);
      const keysetKeys = keysResp.keysets[0].keys;

      const now = Math.floor(Date.now() / 1000);
      const expiry = now + 3600;

      const keysetInfoJson = toKeysetInfoJson(keysetId, keysetKeys, inputFeePpk);
      const fundingTokenAmount = Number(wasm().compute_funding_token_amount(
        BigInt(capacitySat), keysetInfoJson, BigInt(maxPerOutput),
      ));

      const params = {
        mint: mint.MINT_URL,
        unit: "sat",
        capacity: capacitySat,
        fundingTokenAmount,
        keysetId,
        inputFeePpk,
        maximumAmount: maxPerOutput,
        setupTimestamp: now,
        senderPubkey: this.pubKeyHex,
        receiverPubkey: charliePubKeyHex,
        expiryTimestamp: expiry,
      };

      const paramsJson = toParamsJson(params);
      const channelId = wasm().channel_parameters_get_channel_id(paramsJson, channelSecretHex, keysetInfoJson);

      this.channel = createChannel({
        channelId,
        channelSecret: channelSecretHex,
        capacity: capacitySat,
        params,
      });

      this._paramsJson = paramsJson;
      this._keysetInfoJson = keysetInfoJson;
      this._keysetId = keysetId;
      this._inputFeePpk = inputFeePpk;

      return { channelId, channelSecret: channelSecretHex, params };
    },

    async fundChannel() {
      if (!this.channel) throw new Error("No open channel");

      const fundingAmount = this.channel.params.fundingTokenAmount;

      const quote = await mint.postMintQuoteBolt11(fundingAmount);
      await mint.pollMintQuote(quote.quote);

      const fundingJson = wasm().create_funding_outputs(
        this._paramsJson, this.privKeyHex, this._keysetInfoJson,
      );
      const funding = JSON.parse(fundingJson);

      const outputs = funding.blinded_messages.map(bm => ({
        amount: bm.amount, B_: bm.B_, id: this._keysetId,
      }));

      const mintResp = await mint.postMintBolt11({ quote: quote.quote, outputs });

      const proofsJson = wasm().construct_proofs(
        JSON.stringify(mintResp.signatures),
        JSON.stringify(funding.secrets_with_blinding),
        this._keysetInfoJson,
      );
      const proofs = JSON.parse(proofsJson);

      this.proofs = proofs;
      this._fundingProofsJson = JSON.stringify(proofs);
      transitionToFunded(this.channel, proofs);

      return proofs;
    },

    createPayment(amountSat) {
      if (!this.channel || this.channel.status !== STATUS.FUNDED) {
        throw new Error("Channel not funded");
      }

      const balance = this.channel.balanceToReceiver + amountSat;
      const resultJson = wasm().spilman_channel_sender_create_signed_balance_update(
        this._paramsJson,
        this._keysetInfoJson,
        this.privKeyHex,
        this._fundingProofsJson,
        BigInt(balance),
      );
      const result = JSON.parse(resultJson);

      const signedUpdate = {
        messageHex: result.channel_id,
        signatureHex: result.signature,
        tweakedPubHex: "",
      };

      applyPayment(this.channel, amountSat, signedUpdate);

      return signedUpdate;
    },

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

    async acceptChannel(alicePubKeyHex, channelParams) {
      const channelSecretHex = wasm().compute_channel_secret(this.privKeyHex, alicePubKeyHex);

      const keysetsResp = await mint.getKeysets();
      const activeSat = keysetsResp.keysets.find(ks => ks.unit === "sat" && ks.active);
      const keysResp = await mint.getKeys(activeSat.id);
      const keysetKeys = keysResp.keysets[0].keys;
      const inputFeePpk = activeSat.input_fee_ppk || 0;

      const paramsJson = toParamsJson(channelParams);
      const keysetInfoJson = toKeysetInfoJson(channelParams.keysetId, keysetKeys, inputFeePpk);
      const channelId = wasm().channel_parameters_get_channel_id(paramsJson, channelSecretHex, keysetInfoJson);

      this.channel = createChannel({
        channelId,
        channelSecret: channelSecretHex,
        capacity: channelParams.capacity,
        params: channelParams,
      });

      this._channelSecret = crypto.hexToBytes(channelSecretHex);
      this._paramsJson = paramsJson;
      this._keysetInfoJson = keysetInfoJson;
      this._keysetId = channelParams.keysetId;
      this._inputFeePpk = inputFeePpk;

      return { channelId, channelSecret: channelSecretHex };
    },

    acceptFunding(fundingProofs, alicePrivKeyHex) {
      if (!this.channel) throw new Error("No channel");
      this._fundingProofsJson = JSON.stringify(fundingProofs);
      this._alicePrivKeyHex = alicePrivKeyHex;
      transitionToFunded(this.channel, fundingProofs);
    },

    acceptPayment(deltaSat, signedUpdate) {
      if (!this.channel) throw new Error("No channel");
      applyPayment(this.channel, deltaSat, signedUpdate);
    },

    async cooperativeClose() {
      if (!this.channel || this.channel.status !== STATUS.FUNDED) {
        throw new Error("Channel not funded");
      }

      transitionToClosing(this.channel);

      const balanceToCharlie = this.channel.balanceToReceiver;
      const inputTotal = this.channel.fundingProofs.reduce((s, p) => s + p.amount, 0);
      const fee = Math.ceil(inputTotal * (this.channel.params.inputFeePpk || 0) / 1000);
      const balanceToAlice = this.channel.capacity - balanceToCharlie - fee;
      const maxPerOutput = this.channel.params.maximumAmount;
      const channelId = this.channel.id;
      const channelSecret = this._channelSecret;

      const charlieAmounts = balanceToCharlie > 0
        ? crypto.getDenominationAmounts(balanceToCharlie, maxPerOutput)
        : [];
      const charlieOutputs = [];
      const charlieSecrets = [];
      for (let i = 0; i < charlieAmounts.length; i++) {
        const output = crypto.createDeterministicOutput(
          channelSecret, channelId, "receiver", charlieAmounts[i], i,
        );
        charlieOutputs.push({ amount: charlieAmounts[i], B_: output.B_, id: this._keysetId });
        charlieSecrets.push({
          secret: output.secret,
          blinding_factor: output.blindingFactor,
          amount: charlieAmounts[i],
        });
      }

      const aliceAmounts = balanceToAlice > 0
        ? crypto.getDenominationAmounts(balanceToAlice, maxPerOutput)
        : [];
      const aliceOutputs = [];
      const aliceSecrets = [];
      for (let i = 0; i < aliceAmounts.length; i++) {
        const output = crypto.createDeterministicOutput(
          channelSecret, channelId, "sender", aliceAmounts[i], i,
        );
        aliceOutputs.push({ amount: aliceAmounts[i], B_: output.B_, id: this._keysetId });
        aliceSecrets.push({
          secret: output.secret,
          blinding_factor: output.blindingFactor,
          amount: aliceAmounts[i],
        });
      }

      const inputs = this.channel.fundingProofs.map(p => ({
        amount: p.amount,
        id: p.id,
        secret: p.secret,
        C: p.C,
      }));

      const allOutputs = [...charlieOutputs, ...aliceOutputs];

      const channelSecretHex = crypto.bytesToHex(channelSecret);
      const senderTweak = deriveBlindingScalar(channelSecretHex, channelId, "sender_stage1");
      const receiverTweak = deriveBlindingScalar(channelSecretHex, channelId, "receiver_stage1");

      const sigAllMsg = computeSigAllMessage(inputs, allOutputs);
      const sigAllMsgHash = sha256Hex(sigAllMsg);

      const aliceSig = wasm().sign_with_tweaked_key(
        this._alicePrivKeyHex, sigAllMsgHash, senderTweak,
      );
      const charlieSig = wasm().sign_with_tweaked_key(
        this.privKeyHex, sigAllMsgHash, receiverTweak,
      );

      const witness = JSON.stringify({ signatures: [aliceSig, charlieSig] });

      const inputsWithWitness = inputs.map(input => ({
        ...input,
        witness,
      }));

      const swapResp = await mint.postSwap({
        inputs: inputsWithWitness,
        outputs: allOutputs,
      });

      const charlieSigs = swapResp.signatures.slice(0, charlieOutputs.length);
      const aliceSigs = swapResp.signatures.slice(charlieOutputs.length);

      const charlieProofsJson = wasm().construct_proofs(
        JSON.stringify(charlieSigs),
        JSON.stringify(charlieSecrets),
        this._keysetInfoJson,
      );
      const charlieProofs = JSON.parse(charlieProofsJson);

      let aliceProofs = [];
      if (aliceSigs.length > 0) {
        const aliceProofsJson = wasm().construct_proofs(
          JSON.stringify(aliceSigs),
          JSON.stringify(aliceSecrets),
          this._keysetInfoJson,
        );
        aliceProofs = JSON.parse(aliceProofsJson);
      }

      this.proofs = charlieProofs;

      transitionToClosed(this.channel, charlieProofs, aliceProofs);

      return {
        charlieProofs,
        aliceRefundProofs: aliceProofs,
        charlieTotal: charlieProofs.reduce((s, p) => s + p.amount, 0),
        aliceTotal: aliceProofs.reduce((s, p) => s + p.amount, 0),
      };
    },

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

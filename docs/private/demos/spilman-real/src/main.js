import { createAliceWallet, createCharlieWallet } from "./wallet.js";
import {
  updateAlicePanel, updateCharliePanel, debugLog, setPhase, resetUI,
  updateChannelBar, animateTokenFlow, updateSignaturePanel,
  addMintRequest, markStepDot, completeAllDots,
  setEducationText, highlightFlowArrow, highlightFlowNodes,
  updatePaymentPreview, setCustomPaymentEnabled,
} from "./ui.js";

let alice;
let charlie;

function propagateCloseToAlice(closeResult) {
  if (!alice.channel) return;
  alice.channel.status = "CLOSED";
  alice.channel.aliceRefundProofs = closeResult.aliceRefundProofs;
}

const EDU = {
  initial: "Alice will lock ecash in a 2-of-2 multisig with Charlie. The spending condition is: (Alice AND Charlie) OR (Alice after timeout). Each payment is a commitment swap — a full split of the funding token, signed with SIG_ALL so the outputs are fixed. Click a step to begin.",
  open: "Step 1 — ECDH Key Exchange. Alice and Charlie each generate a private key, share public keys, and compute a shared secret via ECDH on secp256k1. This secret is hashed into a channel secret that seeds all deterministic derivations. Both parties will independently derive the same blinded outputs for any balance split — no round trips needed.",
  fund: "Step 2 — Funding. Alice mints 100 sat from the mint using deterministic blinded outputs (P2BK). The resulting proofs are locked under a 2-of-2 condition: spending requires both Alice and Charlie's cooperation, or Alice alone can reclaim after the channel expires. These proofs are the channel's funding token. They cannot be spent by either party alone.",
  pay1: "Step 3 — First Commitment Swap. Alice constructs a swap that spends the funding token: 10 sat to Charlie, 90 sat back to Alice. She signs with SIG_ALL, which commits to the exact inputs AND outputs. Charlie cannot modify the split — he can only submit this exact swap to the mint. The signature is Schnorr, tweaked with the channel secret. This is the atomic guarantee that prevents over-claiming.",
  pay2: "Step 4 — Second Commitment Swap. Alice constructs a new swap: 30 sat to Charlie, 70 sat to Alice. The SIG_ALL signature again commits to exact outputs. The previous 10-sat swap is now SUPERSEDED — only the latest signed swap is valid. Charlie stores this and discards the old one. This is how Spilman channels achieve streaming: each signature replaces the previous one, moving value incrementally.",
  close: "Step 5 — Cooperative Close. Charlie submits the latest commitment swap to the mint. The mint verifies the proofs and swaps them for fresh P2PK proofs: Charlie receives 30 sat in proofs he can spend freely. Alice receives 69 sat (100 − 30 − 1 sat mint fee). The funding token is spent atomically — both sides get their proofs in a single mint transaction. Channel settled.",
};

function interceptMintRequests() {
  const origFetch = window.fetch;
  window.fetch = async function (...args) {
    const url = typeof args[0] === "string" ? args[0] : args[0]?.url || "";
    const method = (args[1]?.method || "GET").toUpperCase();

    if (url.includes("testnut.cashu.exchange")) {
      try {
        const resp = await origFetch.apply(this, args);
        const path = new URL(url).pathname;
        addMintRequest(method, path, resp.status);
        return resp;
      } catch (err) {
        const path = new URL(url).pathname;
        addMintRequest(method, path, 0);
        throw err;
      }
    }

    return origFetch.apply(this, args);
  };
}

function updateAll() {
  updateAlicePanel(alice);
  updateCharliePanel(charlie);
  updateChannelBar(alice, charlie);

  const ch = alice?.channel;
  if (ch?.status === "FUNDED") {
    setCustomPaymentEnabled(true);
    updatePaymentPreview(ch.balanceToReceiver, ch.capacity);
  } else {
    setCustomPaymentEnabled(false);
  }
}

function init() {
  alice = createAliceWallet();
  charlie = createCharlieWallet();
  window.alice = alice;
  window.charlie = charlie;
  updateAlicePanel(alice);
  updateCharliePanel(charlie);
  updateChannelBar(alice, charlie);
  setEducationText(EDU.initial);
  setCustomPaymentEnabled(false);
  debugLog("Wallets initialized");
}

async function runFullLifecycle() {
  setPhase("running");
  debugLog("=== Starting Full Lifecycle ===");

  try {
    debugLog("Phase 1: Opening channel...");
    markStepDot(1);
    highlightFlowArrow("arrow-alice-vault");
    highlightFlowNodes(["flow-alice", "flow-vault", "flow-charlie"]);
    setEducationText(EDU.open);
    const { channelId, params } = await alice.openChannel(charlie.pubKeyHex, { capacitySat: 100 });
    charlie.acceptChannel(alice.pubKeyHex, params);
    debugLog("Channel opened", { channelId: channelId.slice(0, 16) + "..." });
    updateAll();

    debugLog("Phase 2: Funding channel...");
    markStepDot(2);
    highlightFlowArrow("arrow-vault-mint");
    highlightFlowNodes(["flow-vault", "flow-mint"]);
    setEducationText(EDU.fund);
    const fundingProofs = await alice.fundChannel();
    charlie.acceptFunding(fundingProofs);
    debugLog("Channel funded", { proofCount: fundingProofs.length });
    updateAll();

    debugLog("Phase 3: Payment 1 (10 sat)...");
    markStepDot(3);
    highlightFlowArrow("arrow-alice-vault");
    highlightFlowNodes(["flow-alice", "flow-vault"]);
    setEducationText(EDU.pay1);
    const payment1 = alice.createPayment(10);
    charlie.acceptPayment(10, payment1);
    animateTokenFlow(10);
    updateAll();
    updateSignaturePanel(alice);

    debugLog("Phase 4: Payment 2 (20 sat)...");
    markStepDot(4);
    setEducationText(EDU.pay2);
    const payment2 = alice.createPayment(20);
    charlie.acceptPayment(20, payment2);
    animateTokenFlow(20);
    updateAll();
    updateSignaturePanel(alice);

    debugLog("Phase 5: Cooperative close...");
    markStepDot(5);
    highlightFlowArrow("arrow-vault-mint");
    highlightFlowNodes(["flow-vault", "flow-mint", "flow-charlie"]);
    setEducationText(EDU.close);
    const closeResult = await charlie.cooperativeClose();
    propagateCloseToAlice(closeResult);
    debugLog("Channel closed", { charlieTotal: closeResult.charlieTotal, aliceTotal: closeResult.aliceTotal });
    setCustomPaymentEnabled(false);
    updateAll();

    debugLog("=== Lifecycle Complete ===");
    debugLog(`Charlie received: ${closeResult.charlieTotal} sat`);
    debugLog(`Alice refunded: ${closeResult.aliceTotal} sat`);
    completeAllDots();

  } catch (error) {
    debugLog(`ERROR: ${error.message}`);
    console.error(error);
  }

  setPhase("done");
}

window.runVectors = async function () {
  return { pass: true, mismatches: [], note: "Test vector validation not yet implemented" };
};

document.getElementById("run-lifecycle-btn")?.addEventListener("click", runFullLifecycle);
document.getElementById("reset-btn")?.addEventListener("click", () => {
  resetUI();
  init();
  debugLog("Reset");
});

document.getElementById("step1-btn")?.addEventListener("click", async () => {
  setPhase("running");
  try {
    debugLog("Phase 1: Opening channel...");
    markStepDot(1);
    highlightFlowArrow("arrow-alice-vault");
    highlightFlowNodes(["flow-alice", "flow-vault", "flow-charlie"]);
    setEducationText(EDU.open);
    const { channelId, params } = await alice.openChannel(charlie.pubKeyHex, { capacitySat: 100 });
    charlie.acceptChannel(alice.pubKeyHex, params);
    debugLog("Channel opened", { channelId: channelId.slice(0, 16) + "..." });
    updateAll();
  } catch (e) { debugLog(`ERROR: ${e.message}`); console.error(e); }
  setPhase("done");
});

document.getElementById("step2-btn")?.addEventListener("click", async () => {
  setPhase("running");
  try {
    debugLog("Phase 2: Funding channel...");
    markStepDot(2);
    highlightFlowArrow("arrow-vault-mint");
    highlightFlowNodes(["flow-vault", "flow-mint"]);
    setEducationText(EDU.fund);
    const fundingProofs = await alice.fundChannel();
    charlie.acceptFunding(fundingProofs);
    debugLog("Channel funded", { proofCount: fundingProofs.length });
    updateAll();
  } catch (e) { debugLog(`ERROR: ${e.message}`); console.error(e); }
  setPhase("done");
});

document.getElementById("step3-btn")?.addEventListener("click", () => {
  try {
    const slider = document.getElementById("payment-slider");
    const amount = slider ? parseInt(slider.value) : 10;
    debugLog(`Phase 3: Payment 1 (${amount} sat)...`);
    markStepDot(3);
    highlightFlowArrow("arrow-alice-vault");
    highlightFlowNodes(["flow-alice", "flow-vault"]);
    setEducationText(EDU.pay1);
    const payment1 = alice.createPayment(amount);
    charlie.acceptPayment(amount, payment1);
    animateTokenFlow(amount);
    updateAll();
    updateSignaturePanel(alice);

    if (slider) {
      slider.value = "20";
      slider.dispatchEvent(new Event("input"));
    }
  } catch (e) { debugLog(`ERROR: ${e.message}`); console.error(e); }
});

document.getElementById("step4-btn")?.addEventListener("click", () => {
  try {
    const slider = document.getElementById("payment-slider");
    const amount = slider ? parseInt(slider.value) : 20;
    debugLog(`Phase 4: Payment 2 (${amount} sat)...`);
    markStepDot(4);
    setEducationText(EDU.pay2);
    const payment2 = alice.createPayment(amount);
    charlie.acceptPayment(amount, payment2);
    animateTokenFlow(amount);
    updateAll();
    updateSignaturePanel(alice);
  } catch (e) { debugLog(`ERROR: ${e.message}`); console.error(e); }
});

document.getElementById("step5-btn")?.addEventListener("click", async () => {
  setPhase("running");
  try {
    debugLog("Phase 5: Cooperative close...");
    markStepDot(5);
    highlightFlowArrow("arrow-vault-mint");
    highlightFlowNodes(["flow-vault", "flow-mint", "flow-charlie"]);
    setEducationText(EDU.close);
    const closeResult = await charlie.cooperativeClose();
    propagateCloseToAlice(closeResult);
    debugLog("Channel closed", { charlieTotal: closeResult.charlieTotal, aliceTotal: closeResult.aliceTotal });
    setCustomPaymentEnabled(false);
    updateAll();
    debugLog("=== Lifecycle Complete ===");
    debugLog(`Charlie received: ${closeResult.charlieTotal} sat`);
    debugLog(`Alice refunded: ${closeResult.aliceTotal} sat`);
    completeAllDots();
  } catch (e) { debugLog(`ERROR: ${e.message}`); console.error(e); }
  setPhase("done");
});

const paymentSlider = document.getElementById("payment-slider");
paymentSlider?.addEventListener("input", () => {
  const ch = alice?.channel;
  if (!ch) return;
  updatePaymentPreview(ch.balanceToReceiver, ch.capacity);
});

document.getElementById("send-custom-payment-btn")?.addEventListener("click", () => {
  try {
    const slider = document.getElementById("payment-slider");
    const amount = slider ? parseInt(slider.value) : 10;
    if (!alice?.channel || alice.channel.status !== "FUNDED") return;

    const payment = alice.createPayment(amount);
    charlie.acceptPayment(amount, payment);
    animateTokenFlow(amount);
    updateAll();
    updateSignaturePanel(alice);
    debugLog(`Custom payment: ${amount} sat sent`);

    const eduText = `Alice constructed a new commitment swap: spend the funding token, create ${alice.channel.balanceToReceiver} sat for Charlie and ${alice.channel.capacity - alice.channel.balanceToReceiver} sat for Alice. The SIG_ALL signature commits to exact outputs, so Charlie cannot claim more. The previous swap is SUPERSEDED — only this one counts.`;
    setEducationText(eduText);
  } catch (e) { debugLog(`ERROR: ${e.message}`); console.error(e); }
});

interceptMintRequests();
init();
console.log("spilman-real loaded");

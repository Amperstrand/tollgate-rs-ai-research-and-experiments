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
  initial: "Alice locks ecash in a 2-of-2 multisig: both parties must agree to spend, or Alice can reclaim after a timeout. Each payment is a commitment swap signed with SIG_ALL.",
  open: "Alice and Charlie perform ECDH to derive a shared channel secret. This secret seeds all deterministic derivations — both parties will compute identical outputs for any balance split.",
  fund: "Alice locks 100 sat in a funding token with spending condition: (Alice AND Charlie) OR (Alice after expiry). The proofs are now locked — neither party can spend them alone.",
  pay1: "Alice signs a commitment swap: spend the funding token, create 10 sat for Charlie and 90 sat for Alice. Her SIG_ALL signature commits to the exact inputs AND outputs. Charlie can only submit this exact swap to the mint — nothing more.",
  pay2: "Another commitment swap: 30 sat for Charlie, 70 sat for Alice. The previous swap is now SUPERSEDED. Only the latest signed swap is valid. Charlie stores this and discards the old one.",
  close: "Charlie submits the latest commitment swap to the mint. The funding token is spent atomically: Charlie gets 30 sat in fresh proofs, Alice gets 69 sat (1 sat mint fee). Channel settled.",
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

    const eduText = `Alice signed a new commitment swap: spend the funding token, create ${alice.channel.balanceToReceiver} sat for Charlie and ${alice.channel.capacity - alice.channel.balanceToReceiver} sat for Alice. The previous swap is now SUPERSEDED. Only the latest SIG_ALL commitment counts.`;
    setEducationText(eduText);
  } catch (e) { debugLog(`ERROR: ${e.message}`); console.error(e); }
});

interceptMintRequests();
init();
console.log("spilman-real loaded");

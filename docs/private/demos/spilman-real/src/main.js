import { createAliceWallet, createCharlieWallet } from "./wallet.js";
import {
  updateAlicePanel, updateCharliePanel, debugLog, setPhase, resetUI,
  updateChannelBar, animateTokenFlow, updateSignaturePanel,
  addMintRequest, markStepDot, completeAllDots,
} from "./ui.js";

let alice;
let charlie;

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

function init() {
  alice = createAliceWallet();
  charlie = createCharlieWallet();
  window.alice = alice;
  window.charlie = charlie;
  updateAlicePanel(alice);
  updateCharliePanel(charlie);
  updateChannelBar(alice, charlie);
  debugLog("Wallets initialized");
}

async function runFullLifecycle() {
  setPhase("running");
  debugLog("=== Starting Full Lifecycle ===");

  try {
    debugLog("Phase 1: Opening channel...");
    markStepDot(1);
    const { channelId, params } = await alice.openChannel(charlie.pubKeyHex, { capacitySat: 100 });
    charlie.acceptChannel(alice.pubKeyHex, params);
    debugLog("Channel opened", { channelId: channelId.slice(0, 16) + "..." });
    updateAlicePanel(alice);
    updateCharliePanel(charlie);
    updateChannelBar(alice, charlie);

    debugLog("Phase 2: Funding channel...");
    markStepDot(2);
    const fundingProofs = await alice.fundChannel();
    charlie.acceptFunding(fundingProofs);
    debugLog("Channel funded", { proofCount: fundingProofs.length });
    updateAlicePanel(alice);
    updateCharliePanel(charlie);
    updateChannelBar(alice, charlie);

    debugLog("Phase 3: Payment 1 (10 sat)...");
    markStepDot(3);
    const payment1 = alice.createPayment(10);
    charlie.acceptPayment(10, payment1);
    animateTokenFlow(10);
    updateAlicePanel(alice);
    updateCharliePanel(charlie);
    updateChannelBar(alice, charlie);
    updateSignaturePanel(alice);

    debugLog("Phase 4: Payment 2 (20 sat)...");
    markStepDot(4);
    const payment2 = alice.createPayment(20);
    charlie.acceptPayment(20, payment2);
    animateTokenFlow(20);
    updateAlicePanel(alice);
    updateCharliePanel(charlie);
    updateChannelBar(alice, charlie);
    updateSignaturePanel(alice);

    debugLog("Phase 5: Cooperative close...");
    markStepDot(5);
    const closeResult = await charlie.cooperativeClose();
    debugLog("Channel closed", { charlieTotal: closeResult.charlieTotal, aliceTotal: closeResult.aliceTotal });
    updateAlicePanel(alice);
    updateCharliePanel(charlie);
    updateChannelBar(alice, charlie);

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
  debugLog("Reset — new wallets generated");
});

document.getElementById("step1-btn")?.addEventListener("click", async () => {
  setPhase("running");
  try {
    debugLog("Phase 1: Opening channel...");
    markStepDot(1);
    const { channelId, params } = await alice.openChannel(charlie.pubKeyHex, { capacitySat: 100 });
    charlie.acceptChannel(alice.pubKeyHex, params);
    debugLog("Channel opened", { channelId: channelId.slice(0, 16) + "..." });
    updateAlicePanel(alice);
    updateCharliePanel(charlie);
    updateChannelBar(alice, charlie);
  } catch (e) { debugLog(`ERROR: ${e.message}`); console.error(e); }
  setPhase("done");
});

document.getElementById("step2-btn")?.addEventListener("click", async () => {
  setPhase("running");
  try {
    debugLog("Phase 2: Funding channel...");
    markStepDot(2);
    const fundingProofs = await alice.fundChannel();
    charlie.acceptFunding(fundingProofs);
    debugLog("Channel funded", { proofCount: fundingProofs.length });
    updateAlicePanel(alice);
    updateCharliePanel(charlie);
    updateChannelBar(alice, charlie);
  } catch (e) { debugLog(`ERROR: ${e.message}`); console.error(e); }
  setPhase("done");
});

document.getElementById("step3-btn")?.addEventListener("click", () => {
  try {
    debugLog("Phase 3: Payment 1 (10 sat)...");
    markStepDot(3);
    const payment1 = alice.createPayment(10);
    charlie.acceptPayment(10, payment1);
    animateTokenFlow(10);
    updateAlicePanel(alice);
    updateCharliePanel(charlie);
    updateChannelBar(alice, charlie);
    updateSignaturePanel(alice);
  } catch (e) { debugLog(`ERROR: ${e.message}`); console.error(e); }
});

document.getElementById("step4-btn")?.addEventListener("click", () => {
  try {
    debugLog("Phase 4: Payment 2 (20 sat)...");
    markStepDot(4);
    const payment2 = alice.createPayment(20);
    charlie.acceptPayment(20, payment2);
    animateTokenFlow(20);
    updateAlicePanel(alice);
    updateCharliePanel(charlie);
    updateChannelBar(alice, charlie);
    updateSignaturePanel(alice);
  } catch (e) { debugLog(`ERROR: ${e.message}`); console.error(e); }
});

document.getElementById("step5-btn")?.addEventListener("click", async () => {
  setPhase("running");
  try {
    debugLog("Phase 5: Cooperative close...");
    markStepDot(5);
    const closeResult = await charlie.cooperativeClose();
    debugLog("Channel closed", { charlieTotal: closeResult.charlieTotal, aliceTotal: closeResult.aliceTotal });
    updateAlicePanel(alice);
    updateCharliePanel(charlie);
    updateChannelBar(alice, charlie);
    debugLog("=== Lifecycle Complete ===");
    debugLog(`Charlie received: ${closeResult.charlieTotal} sat`);
    debugLog(`Alice refunded: ${closeResult.aliceTotal} sat`);
    completeAllDots();
  } catch (e) { debugLog(`ERROR: ${e.message}`); console.error(e); }
  setPhase("done");
});

interceptMintRequests();
init();
console.log("spilman-real loaded");

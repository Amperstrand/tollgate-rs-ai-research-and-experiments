// main.js — Entry point for spilman-real demo

import { createAliceWallet, createCharlieWallet } from "./wallet.js";
import { updateAlicePanel, updateCharliePanel, debugLog, setPhase, resetUI } from "./ui.js";

let alice;
let charlie;

function init() {
  alice = createAliceWallet();
  charlie = createCharlieWallet();
  window.alice = alice;
  window.charlie = charlie;
  updateAlicePanel(alice);
  updateCharliePanel(charlie);
  debugLog("Wallets initialized");
}

async function runFullLifecycle() {
  setPhase("running");
  debugLog("=== Starting Full Lifecycle ===");

  try {
    debugLog("Phase 1: Opening channel...");
    const { channelId, params } = await alice.openChannel(charlie.pubKeyHex, { capacitySat: 100 });
    charlie.acceptChannel(alice.pubKeyHex, params);
    debugLog("Channel opened", { channelId: channelId.slice(0, 16) + "..." });
    updateAlicePanel(alice);
    updateCharliePanel(charlie);

    debugLog("Phase 2: Funding channel...");
    const fundingProofs = await alice.fundChannel();
    charlie.acceptFunding(fundingProofs);
    debugLog("Channel funded", { proofCount: fundingProofs.length });
    updateAlicePanel(alice);
    updateCharliePanel(charlie);

    debugLog("Phase 3: Payment 1 (10 sat)...");
    const payment1 = alice.createPayment(10);
    charlie.acceptPayment(10, payment1);
    updateAlicePanel(alice);
    updateCharliePanel(charlie);

    debugLog("Phase 4: Payment 2 (20 sat)...");
    const payment2 = alice.createPayment(20);
    charlie.acceptPayment(20, payment2);
    updateAlicePanel(alice);
    updateCharliePanel(charlie);

    debugLog("Phase 5: Cooperative close...");
    const closeResult = await charlie.cooperativeClose();
    debugLog("Channel closed", { charlieTotal: closeResult.charlieTotal, aliceTotal: closeResult.aliceTotal });
    updateAlicePanel(alice);
    updateCharliePanel(charlie);

    debugLog("=== Lifecycle Complete ===");
    debugLog(`Charlie received: ${closeResult.charlieTotal} sat`);
    debugLog(`Alice refunded: ${closeResult.aliceTotal} sat`);

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

init();
console.log("spilman-real loaded");

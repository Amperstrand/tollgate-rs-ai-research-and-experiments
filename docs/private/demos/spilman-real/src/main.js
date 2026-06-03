import { createAliceWallet, createCharlieWallet } from "./wallet.js";
import { initCdkWasm } from "./cdk-wasm-bridge.js";
import {
  updateAlicePanel, updateCharliePanel, debugLog, setPhase, resetUI,
  updateChannelBar, animateTokenFlow, updateSignaturePanel,
  addMintRequest, markStepDot, completeAllDots,
  setEducationText, highlightFlowArrow, highlightFlowNodes,
  updatePaymentPreview, setCustomPaymentEnabled,
  resetMeter,
} from "./ui.js";
import { runTestVectors } from "./test-vectors.js";
import { runCdkVectors } from "./test-vectors-cdk.js";
import { MeterController } from "./meter.js";

let alice;
let charlie;
let meter;

function propagateCloseToAlice(closeResult) {
  if (!alice.channel) return;
  alice.channel.status = "CLOSED";
  alice.channel.aliceRefundProofs = closeResult.aliceRefundProofs;
  alice.proofs = closeResult.aliceRefundProofs;
}

const EDU = {
  initial: "Alice will lock ecash in a 2-of-2 multisig with Charlie. The spending condition is: (Alice AND Charlie) OR (Alice after timeout). Each payment is a commitment swap — Alice signs a full split of the funding token with SIG_ALL so the outputs are fixed. No proofs are created during payments — only signatures. The proofs only get split at settlement. Click a step to begin.",
  open: "Step 1 — ECDH Key Exchange. Alice and Charlie each generate a private key, share public keys, and compute a shared secret via ECDH on secp256k1. This secret is hashed into a channel secret that seeds all deterministic derivations. Both parties will independently derive the same blinded outputs for any balance split — no round trips needed.",
  fund: "Step 2 — Funding. Alice mints 100 sat from the mint using deterministic blinded outputs (P2BK). The resulting proofs [64, 32, 4] are the channel's funding token — locked under a 2-of-2 condition. Why these three? Cashu uses binary denominations: 100 = 64 + 32 + 4. The mint holds keys for all powers of 2, so any amount can be represented at settlement. During the channel, no proofs are split — payments are just signatures.",
  pay1: "Step 3 — First Commitment Swap. Alice constructs a swap that spends the funding token: 10 sat to Charlie, 90 sat back to Alice. She signs with SIG_ALL, which commits to the exact inputs AND outputs. No mint interaction needed — this is just a Schnorr signature on the full swap specification. Charlie can verify the signature by reconstructing the same swap from the channel secret. This is the atomic guarantee that prevents over-claiming.",
  pay2: "Step 4 — Second Commitment Swap. Alice constructs a new swap: 30 sat to Charlie, 70 sat to Alice. The SIG_ALL signature again commits to exact outputs. The previous 10-sat swap is now SUPERSEDED — only the latest signed swap is valid. Charlie stores this and discards the old one. This is how Spilman channels achieve streaming: each signature replaces the previous one, moving value incrementally — no mint contact needed.",
  close: "Step 5 — Cooperative Close. Charlie submits the latest commitment swap to the mint. NOW the proofs get split: the mint takes the funding token [64, 32, 4] as inputs and creates fresh proofs in whatever denominations are needed — Charlie gets 30 sat, Alice gets 69 sat (100 − 30 − 1 sat mint fee). The funding token is spent atomically — both sides get their proofs in a single mint transaction. Channel settled.",
  unilateral: "Unilateral Close. Charlie closes the channel WITHOUT Alice's cooperation. In the Rust bridge (bridge.rs:1681-1689), unilateral close calls prepare_close_data with validate_due=false — the same swap request as cooperative close, but initiated by the receiver alone using Alice's last signed balance update as proof of the current balance. In production, this lets Charlie settle even if Alice disappears. In this demo, both keys are in memory so the swap mechanics are identical — the difference is who initiates.",
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
    if (meter) meter.enable();
  } else {
    setCustomPaymentEnabled(false);
    if (meter && (ch?.status === "CLOSED" || ch?.status === "CLOSING")) {
      meter.disable();
    }
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

  meter = new MeterController({
    onPayment(amount) {
      const payment = alice.createPayment(amount);
      charlie.acceptPayment(amount, payment);
      animateTokenFlow(amount);
      updateAll();
      updateSignaturePanel(alice);
      debugLog(`Meter auto-payment: ${amount} sat`);
    },
    onDepleted() {
      debugLog("Meter: channel depleted");
      setEducationText("Credit exhausted! The meter consumed all prepaid sats via auto-payments. Use the Pay button to top up — send more sats to Charlie — then click the bulb to resume consumption. This is the TollGate model: pay-as-you-go resource delivery via streaming micropayments.");
    },
    onStatusChange(data) {
      const readout = document.getElementById("meter-readout");
      const dial = document.getElementById("meter-dial");
      const bulb = document.getElementById("meter-bulb");
      const section = document.getElementById("meter-section");

      if (readout) {
        const credit = data.creditRemaining;
        readout.textContent = `${data.watts}W · ${data.satPerSec} sat/sec · ${credit} sat credit · ${data.totalConsumed} sat consumed`;
      }
      if (dial) {
        dial.style.transform = `translate(-50%, 0) rotate(${data.dialAngle}deg)`;
      }
      if (bulb) {
        bulb.classList.toggle("bulb-on", data.isOn);
      }
      if (section && meter) {
        section.classList.toggle("meter-enabled", meter.isEnabled);
      }
    },
    getChannelState() {
      return alice?.channel || null;
    },
  });
}

document.getElementById("meter-bulb")?.addEventListener("click", () => {
  if (!meter) return;
  const ch = alice?.channel;
  if (!ch || ch.status !== "FUNDED") return;
  const credit = ch.capacity - ch.balanceToReceiver;
  if (credit <= 0 && !meter.isOn) {
    setEducationText("No credit remaining. Use the Pay button to top up — send sats to Charlie as prepaid credit. Then click the bulb to start consuming.");
    return;
  }
  meter.toggle();
  if (meter.isOn) {
    setEducationText("Meter ON: Charlie sells electricity at 5 watts for 1 sat per watt-second. The bulb auto-pays at 5 sat/sec through commitment swaps — the same mechanism as manual payments. Alice's credit ticks down in real-time. When it hits zero, the bulb turns off. Click Pay to top up again.");
  }
});

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
    await charlie.acceptChannel(alice.pubKeyHex, params);
    debugLog("Channel opened", { channelId: channelId.slice(0, 16) + "..." });
    updateAll();

    debugLog("Phase 2: Funding channel...");
    markStepDot(2);
    highlightFlowArrow("arrow-vault-mint");
    highlightFlowNodes(["flow-vault", "flow-mint"]);
    setEducationText(EDU.fund);
    const fundingProofs = await alice.fundChannel();
    charlie.acceptFunding(fundingProofs, alice.privKeyHex);
    debugLog("Channel funded", { proofCount: fundingProofs.length });
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
  const result = await runTestVectors();
  console.log("Test vector validation:", result);
  if (result.failures.length > 0) {
    console.error("Failures:");
    for (const f of result.failures) {
      console.error(`  ${f.check}: expected ${f.expected?.slice(0, 32)}... got ${f.actual?.slice(0, 32)}...`);
    }
  }
  return result;
};

document.getElementById("run-lifecycle-btn")?.addEventListener("click", runFullLifecycle);
document.getElementById("reset-btn")?.addEventListener("click", () => {
  resetUI();
  if (meter) meter.reset();
  resetMeter();
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
    await charlie.acceptChannel(alice.pubKeyHex, params);
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
    charlie.acceptFunding(fundingProofs, alice.privKeyHex);
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
    debugLog("Channel closed (cooperative)", { charlieTotal: closeResult.charlieTotal, aliceTotal: closeResult.aliceTotal });
    setCustomPaymentEnabled(false);
    updateAll();
    debugLog("=== Lifecycle Complete ===");
    debugLog(`Charlie received: ${closeResult.charlieTotal} sat`);
    debugLog(`Alice refunded: ${closeResult.aliceTotal} sat`);
    completeAllDots();
  } catch (e) { debugLog(`ERROR: ${e.message}`); console.error(e); }
  setPhase("done");
});

document.getElementById("step5-unilateral-btn")?.addEventListener("click", async () => {
  setPhase("running");
  try {
    debugLog("Unilateral close (Charlie initiates alone)...");
    markStepDot(5);
    highlightFlowArrow("arrow-vault-mint");
    highlightFlowNodes(["flow-vault", "flow-mint", "flow-charlie"]);
    setEducationText(EDU.unilateral);
    const closeResult = await charlie.unilateralClose();
    propagateCloseToAlice(closeResult);
    debugLog("Channel closed (unilateral)", { charlieTotal: closeResult.charlieTotal, aliceTotal: closeResult.aliceTotal });
    setCustomPaymentEnabled(false);
    updateAll();
    debugLog("=== Lifecycle Complete (unilateral) ===");
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

    const remaining = alice.channel.capacity - alice.channel.balanceToReceiver;
    if (amount <= 0 || amount > remaining) {
      setEducationText(`Channel depleted. All ${alice.channel.capacity} sat have been committed to Charlie. No further payments possible — Charlie must close the channel to settle.`);
      return;
    }

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

window.runCdkVectors = async function () {
  const result = await runCdkVectors();
  console.log("cdk-wasm test vector validation:", result);
  if (result.failures.length > 0) {
    console.error("Failures:");
    for (const f of result.failures) {
      console.error(`  ${f.check}: expected ${f.expected}... got ${f.actual}...`);
    }
  }
  return result;
};

interceptMintRequests();
initCdkWasm()
  .then(() => {
    init();
    debugLog("cdk-wasm initialized");
    console.log("spilman-real loaded (cdk-wasm ready)");
  })
  .catch(err => {
    console.error("cdk-wasm init failed:", err);
    init();
    debugLog("cdk-wasm FAILED: " + err.message);
    console.log("spilman-real loaded (cdk-wasm FAILED)");
  });

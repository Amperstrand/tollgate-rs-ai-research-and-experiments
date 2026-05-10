const DENOM_COLORS = {
  64: "denom-64", 32: "denom-32", 16: "denom-16",
  8: "denom-8", 4: "denom-4", 2: "denom-2", 1: "denom-1",
};

const LOCK_CHIP_COLORS = {
  64: "lock-chip-64", 32: "lock-chip-32", 16: "lock-chip-16",
  8: "lock-chip-8", 4: "lock-chip-4", 2: "lock-chip-2", 1: "lock-chip-1",
};

const signatureHistoryData = [];

function truncateHex(hex, chars = 8) {
  if (!hex || hex.length <= chars * 2 + 3) return hex || "";
  return hex.slice(0, chars) + "..." + hex.slice(-chars);
}

function denomClass(amount) {
  return DENOM_COLORS[amount] || "denom-1";
}

function lockChipClass(amount) {
  return LOCK_CHIP_COLORS[amount] || "lock-chip-1";
}

function splitIntoDenoms(total) {
  const result = [];
  let remaining = total;
  for (const d of [64, 32, 16, 8, 4, 2, 1]) {
    while (remaining >= d) {
      result.push(d);
      remaining -= d;
    }
  }
  return result;
}

function renderProofTokens(containerId, proofs) {
  const el = document.getElementById(containerId);
  if (!el) return;
  if (!proofs || proofs.length === 0) {
    el.innerHTML = "";
    return;
  }
  el.innerHTML = proofs.map(p =>
    `<div class="proof-token ${denomClass(p.amount)}" title="${p.amount} sat | secret: ${truncateHex(p.secret, 6)} | C: ${truncateHex(p.C, 6)}">${p.amount}</div>`
  ).join("");
}

export function updateAlicePanel(aliceWallet) {
  const pubkeyEl = document.getElementById("alice-pubkey");
  const channelIdEl = document.getElementById("alice-channel-id");
  const balanceEl = document.getElementById("alice-balance");
  const proofCountEl = document.getElementById("alice-proof-count");
  const proofTotalEl = document.getElementById("alice-proof-total");
  const logEl = document.getElementById("alice-activity-log");

  if (!aliceWallet) return;

  if (pubkeyEl) pubkeyEl.textContent = aliceWallet.pubKeyHex?.slice(0, 32) + "..." || "...";

  const bal = aliceWallet.getBalance();
  if (channelIdEl) channelIdEl.textContent = aliceWallet.channel?.id?.slice(0, 16) + "..." || "...";

  let balanceText;
  if (aliceWallet.channel?.status === "CLOSED" && aliceWallet.channel.aliceRefundProofs) {
    const netTotal = aliceWallet.channel.aliceRefundProofs.reduce((s, p) => s + p.amount, 0);
    balanceText = `${netTotal} sat (CLOSED)`;
  } else {
    balanceText = `${bal.remaining} sat (${bal.status})`;
  }
  if (balanceEl) balanceEl.textContent = balanceText;

  if (proofCountEl) proofCountEl.textContent = String(aliceWallet.proofs?.length || 0);
  if (proofTotalEl) proofTotalEl.textContent = `${(aliceWallet.proofs || []).reduce((s, p) => s + p.amount, 0)} sat`;
  if (logEl) {
    const history = aliceWallet.channel?.history || [];
    logEl.textContent = history.map(h => `[${new Date(h.timestamp).toLocaleTimeString()}] ${h.phase}${h.delta ? " +" + h.delta + " sat" : ""}`).join("\n") || "...";
  }

  renderProofTokens("alice-proofs-detail", aliceWallet.proofs);
}

export function updateCharliePanel(charlieWallet) {
  const pubkeyEl = document.getElementById("charlie-pubkey");
  const channelIdEl = document.getElementById("charlie-channel-id");
  const balanceEl = document.getElementById("charlie-balance");
  const proofCountEl = document.getElementById("charlie-proof-count");
  const proofTotalEl = document.getElementById("charlie-proof-total");
  const logEl = document.getElementById("charlie-activity-log");

  if (!charlieWallet) return;

  if (pubkeyEl) pubkeyEl.textContent = charlieWallet.pubKeyHex?.slice(0, 32) + "..." || "...";

  const bal = charlieWallet.getBalance();
  if (channelIdEl) channelIdEl.textContent = charlieWallet.channel?.id?.slice(0, 16) + "..." || "...";
  if (balanceEl) balanceEl.textContent = `${bal.received} sat (${bal.status})`;
  if (proofCountEl) proofCountEl.textContent = String(charlieWallet.proofs?.length || 0);
  if (proofTotalEl) proofTotalEl.textContent = `${(charlieWallet.proofs || []).reduce((s, p) => s + p.amount, 0)} sat`;
  if (logEl) {
    const history = charlieWallet.channel?.history || [];
    logEl.textContent = history.map(h => `[${new Date(h.timestamp).toLocaleTimeString()}] ${h.phase}${h.delta ? " +" + h.delta + " sat" : ""}`).join("\n") || "...";
  }

  renderProofTokens("charlie-proofs-detail", charlieWallet.proofs);
}

export function debugLog(message, data = null) {
  const el = document.getElementById("debug-dump");
  if (!el) return;
  const time = new Date().toLocaleTimeString();
  const line = data
    ? `[${time}] ${message}: ${JSON.stringify(data, null, 2)}`
    : `[${time}] ${message}`;
  el.textContent += line + "\n";
  el.scrollTop = el.scrollHeight;
}

export function setPhase(phase) {
  const btn = document.getElementById("run-lifecycle-btn");
  if (!btn) return;
  if (phase === "running") {
    btn.disabled = true;
    btn.textContent = "Running...";
  } else if (phase === "done") {
    btn.disabled = false;
    btn.textContent = "Run Full Lifecycle";
  }
}

export function resetUI() {
  const ids = [
    "alice-pubkey", "alice-channel-id", "alice-balance", "alice-proof-count", "alice-proof-total", "alice-activity-log",
    "charlie-pubkey", "charlie-channel-id", "charlie-balance", "charlie-proof-count", "charlie-proof-total", "charlie-activity-log",
  ];
  for (const id of ids) {
    const el = document.getElementById(id);
    if (el) el.textContent = "...";
  }
  const debugEl = document.getElementById("debug-dump");
  if (debugEl) debugEl.textContent = "Ready...\n";

  const proofsAlice = document.getElementById("alice-proofs-detail");
  const proofsCharlie = document.getElementById("charlie-proofs-detail");
  if (proofsAlice) proofsAlice.innerHTML = "";
  if (proofsCharlie) proofsCharlie.innerHTML = "";

  const sigContent = document.getElementById("sig-content");
  if (sigContent) sigContent.innerHTML = '<div class="sig-empty">No commitment yet. Fund the channel to begin.</div>';

  const mintTimeline = document.getElementById("mint-timeline");
  if (mintTimeline) mintTimeline.innerHTML = '<div class="mint-empty">No mint requests yet</div>';

  const badge = document.getElementById("channel-state-badge");
  if (badge) {
    badge.textContent = "INIT";
    badge.className = "channel-state-badge";
  }

  const vaultTokens = document.getElementById("vault-tokens");
  if (vaultTokens) vaultTokens.innerHTML = '<div class="lock-empty-msg">Awaiting funding proofs...</div>';

  const vaultSplitAlice = document.getElementById("vault-split-alice");
  const vaultSplitCharlie = document.getElementById("vault-split-charlie");
  if (vaultSplitAlice) vaultSplitAlice.style.width = "100%";
  if (vaultSplitCharlie) vaultSplitCharlie.style.width = "0%";

  const vaultLabelAlice = document.getElementById("vault-label-alice");
  const vaultLabelCharlie = document.getElementById("vault-label-charlie");
  if (vaultLabelAlice) vaultLabelAlice.textContent = "100 sat (Alice)";
  if (vaultLabelCharlie) vaultLabelCharlie.textContent = "0 sat (Charlie)";

  const vaultStatus = document.getElementById("vault-status-text");
  if (vaultStatus) vaultStatus.textContent = "No funding token yet";

  const capLabel = document.getElementById("channel-capacity-label");
  if (capLabel) capLabel.textContent = "100 sat capacity";

  const eduText = document.getElementById("edu-text");
  if (eduText) eduText.textContent = "Alice will lock ecash in a 2-of-2 multisig with Charlie. The spending condition is: (Alice AND Charlie) OR (Alice after timeout). Each payment is a commitment swap — a full split of the funding token, signed with SIG_ALL so the outputs are fixed. Click a step to begin.";

  signatureHistoryData.length = 0;
  const histList = document.getElementById("sig-history-list");
  if (histList) histList.innerHTML = '<div class="sig-history-empty">No previous commitments</div>';

  const dots = document.querySelectorAll(".step-dot");
  dots.forEach(d => { d.classList.remove("completed", "active"); });

  const customBtn = document.getElementById("send-custom-payment-btn");
  if (customBtn) customBtn.disabled = true;

  document.querySelectorAll(".flow-inline-arrow").forEach(el => {
    el.classList.remove("active", "active-alice");
  });
  document.querySelectorAll(".flow-inline-node").forEach(el => {
    el.classList.remove("active");
  });
}

export function updateChannelBar(aliceWallet, charlieWallet) {
  const aliceCh = aliceWallet?.channel;
  const charlieCh = charlieWallet?.channel;
  const statusOrder = { CLOSED: 4, CLOSING: 3, FUNDED: 2, INIT: 1 };
  const aOrd = statusOrder[aliceCh?.status] || 0;
  const cOrd = statusOrder[charlieCh?.status] || 0;
  const ch = cOrd > aOrd ? charlieCh : (aliceCh || charlieCh);
  if (!ch) return;

  const capacity = ch.capacity;
  const toCharlie = ch.balanceToReceiver;

  let toAlice;
  if (ch.status === "CLOSED" && ch.aliceRefundProofs) {
    toAlice = ch.aliceRefundProofs.reduce((s, p) => s + p.amount, 0);
  } else {
    toAlice = capacity - toCharlie;
  }

  const pctAlice = capacity > 0 ? (toAlice / capacity) * 100 : 100;
  const pctCharlie = capacity > 0 ? (toCharlie / capacity) * 100 : 0;

  const splitAlice = document.getElementById("vault-split-alice");
  const splitCharlie = document.getElementById("vault-split-charlie");
  if (splitAlice) splitAlice.style.width = `${pctAlice}%`;
  if (splitCharlie) splitCharlie.style.width = `${pctCharlie}%`;

  const labelAlice = document.getElementById("vault-label-alice");
  const labelCharlie = document.getElementById("vault-label-charlie");
  if (labelAlice) labelAlice.textContent = `${toAlice} sat (Alice)`;
  if (labelCharlie) labelCharlie.textContent = `${toCharlie} sat (Charlie)`;

  const badge = document.getElementById("channel-state-badge");
  if (badge) {
    const status = ch.status;
    badge.textContent = status;
    badge.className = "channel-state-badge";
    const s = status.toLowerCase();
    if (s === "init") badge.classList.add("state-init");
    else if (s === "funded") badge.classList.add("state-funded");
    else if (s === "closing") badge.classList.add("state-closing");
    else if (s === "closed") badge.classList.add("state-closed");
  }

  const capLabel = document.getElementById("channel-capacity-label");
  if (capLabel) capLabel.textContent = `${capacity} sat capacity`;

  updateLockTokens(ch);

  const vaultStatus = document.getElementById("vault-status-text");
  if (vaultStatus) {
    if (ch.status === "INIT") {
      vaultStatus.textContent = "No funding token yet";
    } else if (ch.status === "FUNDED") {
      vaultStatus.textContent = "Locked by 2-of-2: (Alice + Charlie) OR (Alice after expiry)";
    } else if (ch.status === "CLOSING") {
      vaultStatus.textContent = "Submitting commitment swap to mint...";
    } else if (ch.status === "CLOSED") {
      vaultStatus.textContent = "Funding token spent. Channel settled.";
    }
  }
}

function updateLockTokens(ch) {
  const container = document.getElementById("vault-tokens");
  if (!container) return;

  if (!ch.fundingProofs || ch.fundingProofs.length === 0) {
    if (ch.status === "CLOSED") {
      container.innerHTML = '<div class="lock-empty-msg">Funding token spent</div>';
    } else {
      container.innerHTML = '<div class="lock-empty-msg">Awaiting funding proofs...</div>';
    }
    return;
  }

  if (ch.status === "CLOSED") {
    container.innerHTML = '<div class="lock-empty-msg">Funding token spent. Proofs swapped via commitment.</div>';
    return;
  }

  const proofs = ch.fundingProofs;
  const toCharlie = ch.balanceToReceiver;

  let charlieRemaining = toCharlie;
  const charlieProofs = [];
  const aliceProofs = [];
  for (const p of proofs) {
    if (charlieRemaining >= p.amount) {
      charlieProofs.push(p);
      charlieRemaining -= p.amount;
    } else {
      aliceProofs.push(p);
      charlieRemaining = 0;
    }
  }

  let html = "";

  if (charlieProofs.length > 0) {
    html += `<div class="lock-chip-group lock-group-charlie">
      <div class="lock-group-label">Charlie's claim</div>
      <div class="lock-group-chips">${charlieProofs.map(p =>
        `<div class="lock-chip ${lockChipClass(p.amount)} lock-chip-claimed-charlie" title="${p.amount} sat">${p.amount}</div>`
      ).join("")}</div>
    </div>`;
  }

  if (aliceProofs.length > 0) {
    html += `<div class="lock-chip-group lock-group-alice">
      <div class="lock-group-label">Alice's claim</div>
      <div class="lock-group-chips">${aliceProofs.map(p =>
        `<div class="lock-chip ${lockChipClass(p.amount)} lock-chip-claimed-alice" title="${p.amount} sat">${p.amount}</div>`
      ).join("")}</div>
    </div>`;
  }

  container.innerHTML = html;
}

export function animateTokenFlow(amountSat) {
  const sigCard = document.getElementById("signature-active");
  if (!sigCard) return;

  sigCard.classList.remove("sig-animate");
  void sigCard.offsetWidth;
  sigCard.classList.add("sig-animate");

  setTimeout(() => sigCard.classList.remove("sig-animate"), 500);
}

export function updateSignaturePanel(aliceWallet) {
  const ch = aliceWallet?.channel;
  if (!ch?.lastSignedUpdate) return;

  const sig = ch.lastSignedUpdate;
  const container = document.getElementById("sig-content");
  if (!container) return;

  const prevBalance = ch.balanceToReceiver - (ch.history?.filter(h => h.phase === "PAYMENT").pop()?.delta || 0);

  if (prevBalance > 0) {
    signatureHistoryData.push({
      charlieAmount: prevBalance,
      aliceAmount: ch.capacity - prevBalance,
    });
    renderSignatureHistory();
  }

  const charlieAmt = ch.balanceToReceiver;
  const feePpk = ch.params?.inputFeePpk || 0;
  const fee = Math.ceil(ch.capacity * feePpk / 1000);
  const aliceAmt = ch.capacity - charlieAmt;
  const aliceNet = aliceAmt - fee;

  container.innerHTML = `
    <div class="sig-split-row">
      <div class="sig-split-box sig-split-charlie">
        <div class="sig-split-name">Charlie receives</div>
        <div class="sig-split-amount">${charlieAmt} <span class="sig-split-unit">sat</span></div>
      </div>
      <div class="sig-split-box sig-split-alice">
        <div class="sig-split-name">Alice retains</div>
        <div class="sig-split-amount">${aliceNet} <span class="sig-split-unit">sat</span></div>
        ${fee > 0 ? `<div class="sig-fee-note">gross ${aliceAmt} sat − ${fee} sat fee</div>` : ""}
      </div>
    </div>
    <div class="sig-detail-row">
      <div class="sig-detail-label">Commitment swap</div>
      <div class="sig-detail-value">Inputs: funding (${ch.capacity} sat) \u2192 Outputs: Charlie (${charlieAmt}) + Alice (${aliceNet}${fee > 0 ? `, fee ${fee}` : ""})</div>
    </div>
    <div class="sig-detail-row">
      <div class="sig-detail-label">Message</div>
      <div class="sig-detail-value">SHA256(channel_id | "${charlieAmt}")</div>
    </div>
    <div class="sig-detail-row">
      <div class="sig-detail-label">Message Hash</div>
      <div class="sig-detail-value">${sig.messageHex ? truncateHex(sig.messageHex, 10) : "..."}</div>
    </div>
    <div class="sig-detail-row">
      <div class="sig-detail-label">SIG_ALL Signature</div>
      <div class="sig-detail-value">${sig.signatureHex ? truncateHex(sig.signatureHex, 10) : "..."}</div>
    </div>
    <div class="sig-detail-row">
      <div class="sig-detail-label">Tweaked Public Key</div>
      <div class="sig-detail-value">${sig.tweakedPubHex ? truncateHex(sig.tweakedPubHex, 10) : "..."}</div>
    </div>
    <div class="sig-verified">SIG_ALL verified \u2014 atomic input/output commitment</div>
  `;
}

function renderSignatureHistory() {
  const list = document.getElementById("sig-history-list");
  if (!list) return;

  const emptyEl = list.querySelector(".sig-history-empty");
  if (emptyEl) emptyEl.remove();

  list.innerHTML = signatureHistoryData.map((entry, i) => `
    <div class="sig-history-card">
      <span class="sig-history-label">Superseded</span>
      <span class="sig-history-text">${entry.charlieAmount} sat to Charlie, ${entry.aliceAmount} sat to Alice</span>
    </div>
  `).join("");

  list.scrollTop = list.scrollHeight;
}

export function addMintRequest(method, path, status) {
  const timeline = document.getElementById("mint-timeline");
  if (!timeline) return;

  const empty = timeline.querySelector(".mint-empty");
  if (empty) empty.remove();

  const card = document.createElement("div");
  card.className = "mint-request-card";

  const methodClass = method.toLowerCase() === "get" ? "get" : "post";
  const statusClass = status >= 200 && status < 300 ? "s200" : "s-error";

  card.innerHTML = `
    <span class="mint-method ${methodClass}">${method}</span>
    <span class="mint-path">${path}</span>
    <span class="mint-status ${statusClass}">${status}</span>
  `;

  timeline.appendChild(card);
  timeline.scrollTop = timeline.scrollHeight;
}

export function markStepDot(step) {
  const dots = document.querySelectorAll(".step-dot");
  dots.forEach((d) => {
    const s = parseInt(d.dataset.step);
    if (s < step) d.classList.add("completed");
    else if (s === step) d.classList.add("active");
    else { d.classList.remove("completed", "active"); }
  });
}

export function completeAllDots() {
  const dots = document.querySelectorAll(".step-dot");
  dots.forEach(d => {
    d.classList.remove("active");
    d.classList.add("completed");
  });
}

export function setEducationText(text) {
  const el = document.getElementById("edu-text");
  if (el) el.textContent = text;
}

export function highlightFlowArrow(arrowId) {
  document.querySelectorAll(".flow-inline-arrow").forEach(el => {
    el.classList.remove("active", "active-alice");
  });
  document.querySelectorAll(".flow-inline-node").forEach(el => {
    el.classList.remove("active");
  });

  const arrow = document.getElementById(arrowId);
  if (arrow) arrow.classList.add("active");
}

export function highlightFlowNodes(nodeIds) {
  document.querySelectorAll(".flow-inline-node").forEach(el => {
    el.classList.remove("active");
  });
  for (const id of nodeIds) {
    const node = document.getElementById(id);
    if (node) node.classList.add("active");
  }
}

export function updatePaymentPreview(currentBalance, capacity) {
  const slider = document.getElementById("payment-slider");
  const display = document.getElementById("payment-amount-display");
  const previewCharlie = document.getElementById("preview-charlie");
  const previewAlice = document.getElementById("preview-alice");

  if (!slider) return;

  const amount = parseInt(slider.value);
  if (display) display.textContent = String(amount);
  if (previewCharlie) previewCharlie.textContent = `+${amount} sat`;
  if (previewAlice) previewAlice.textContent = `${capacity - currentBalance - amount} sat`;

  const maxPay = capacity - currentBalance;
  slider.max = String(Math.max(1, maxPay));
  if (amount > maxPay) {
    slider.value = String(maxPay);
    if (display) display.textContent = String(maxPay);
  }
}

export function setCustomPaymentEnabled(enabled) {
  const btn = document.getElementById("send-custom-payment-btn");
  if (btn) btn.disabled = !enabled;
}

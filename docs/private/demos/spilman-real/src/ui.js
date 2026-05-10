const DENOM_COLORS = {
  64: "denom-64", 32: "denom-32", 16: "denom-16",
  8: "denom-8", 4: "denom-4", 2: "denom-2", 1: "denom-1",
};

function truncateHex(hex, chars = 8) {
  if (!hex || hex.length <= chars * 2 + 3) return hex || "";
  return hex.slice(0, chars) + "..." + hex.slice(-chars);
}

function denomClass(amount) {
  return DENOM_COLORS[amount] || "denom-1";
}

function renderProofTokens(containerId, proofs, owner) {
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
  if (balanceEl) balanceEl.textContent = `${bal.remaining} sat (${bal.status})`;
  if (proofCountEl) proofCountEl.textContent = String(aliceWallet.proofs?.length || 0);
  if (proofTotalEl) proofTotalEl.textContent = `${(aliceWallet.proofs || []).reduce((s, p) => s + p.amount, 0)} sat`;
  if (logEl) {
    const history = aliceWallet.channel?.history || [];
    logEl.textContent = history.map(h => `[${new Date(h.timestamp).toLocaleTimeString()}] ${h.phase}${h.delta ? " +" + h.delta + " sat" : ""}`).join("\n") || "...";
  }

  renderProofTokens("alice-proofs-detail", aliceWallet.proofs, "alice");
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

  renderProofTokens("charlie-proofs-detail", charlieWallet.proofs, "charlie");
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
  if (sigContent) sigContent.innerHTML = '<div class="sig-empty">No signature yet</div>';

  const mintTimeline = document.getElementById("mint-timeline");
  if (mintTimeline) mintTimeline.innerHTML = '<div class="mint-empty">No mint requests yet</div>';

  const barAlice = document.getElementById("channel-bar-alice");
  const barCharlie = document.getElementById("channel-bar-charlie");
  if (barAlice) barAlice.style.width = "100%";
  if (barCharlie) barCharlie.style.width = "0%";

  const amtAlice = document.getElementById("bar-alice-amount");
  const amtCharlie = document.getElementById("bar-charlie-amount");
  if (amtAlice) amtAlice.textContent = "100";
  if (amtCharlie) amtCharlie.textContent = "0";

  const badge = document.getElementById("channel-state-badge");
  if (badge) {
    badge.textContent = "INIT";
    badge.className = "channel-state-badge";
  }

  const dots = document.querySelectorAll(".step-dot");
  dots.forEach(d => { d.classList.remove("completed", "active"); });
}

export function updateChannelBar(aliceWallet, charlieWallet) {
  const ch = aliceWallet?.channel || charlieWallet?.channel;
  if (!ch) return;

  const capacity = ch.capacity;
  const toCharlie = ch.balanceToReceiver;
  const toAlice = capacity - toCharlie;

  const pctAlice = capacity > 0 ? (toAlice / capacity) * 100 : 100;
  const pctCharlie = capacity > 0 ? (toCharlie / capacity) * 100 : 0;

  const barAlice = document.getElementById("channel-bar-alice");
  const barCharlie = document.getElementById("channel-bar-charlie");
  if (barAlice) barAlice.style.width = `${pctAlice}%`;
  if (barCharlie) barCharlie.style.width = `${pctCharlie}%`;

  const amtAlice = document.getElementById("bar-alice-amount");
  const amtCharlie = document.getElementById("bar-charlie-amount");
  if (amtAlice) amtAlice.textContent = String(toAlice);
  if (amtCharlie) amtCharlie.textContent = String(toCharlie);

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
}

export function animateTokenFlow(amountSat) {
  const layer = document.getElementById("token-flow-layer");
  if (!layer) return;

  const denoms = splitIntoDenoms(amountSat);
  const barRect = layer.getBoundingClientRect();
  const barWidth = barRect.width;

  denoms.forEach((denom, i) => {
    const pellet = document.createElement("div");
    pellet.className = `token-pellet ${denomClass(denom)}`;
    pellet.textContent = denom;
    pellet.style.top = `${3 + (i % 3) * 2}px`;
    pellet.style.animationDelay = `${i * 120}ms`;
    pellet.classList.add("flying");
    layer.appendChild(pellet);

    pellet.addEventListener("animationend", () => pellet.remove());
  });
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

export function updateSignaturePanel(aliceWallet) {
  const ch = aliceWallet?.channel;
  if (!ch?.lastSignedUpdate) return;

  const sig = ch.lastSignedUpdate;
  const container = document.getElementById("sig-content");
  if (!container) return;

  container.innerHTML = `
    <div class="sig-field">
      <div class="sig-field-label">Message</div>
      <div class="sig-field-value">SHA256(channel_id | "|" | ${ch.balanceToReceiver})</div>
    </div>
    <div class="sig-field">
      <div class="sig-field-label">Message Hash</div>
      <div class="sig-field-value">${sig.messageHex ? truncateHex(sig.messageHex, 10) : "..."}</div>
    </div>
    <div class="sig-field">
      <div class="sig-field-label">Schnorr Signature</div>
      <div class="sig-field-value">${sig.signatureHex ? truncateHex(sig.signatureHex, 10) : "..."}</div>
    </div>
    <div class="sig-field">
      <div class="sig-field-label">Tweaked Public Key</div>
      <div class="sig-field-value">${sig.tweakedPubHex ? truncateHex(sig.tweakedPubHex, 10) : "..."}</div>
    </div>
    <div class="sig-verified">Verified</div>
  `;
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
  dots.forEach((d, i) => {
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

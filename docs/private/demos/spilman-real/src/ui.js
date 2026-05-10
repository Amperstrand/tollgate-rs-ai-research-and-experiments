// ui.js — DOM update functions for spilman-real demo

/** Update Alice's wallet panel */
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
}

/** Update Charlie's wallet panel */
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
}

/** Update the debug panel with a message */
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

/** Set button states based on lifecycle phase */
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

/** Reset all UI elements */
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
}

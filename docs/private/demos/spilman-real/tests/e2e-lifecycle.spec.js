// @ts-check
import { test, expect } from "@playwright/test";
import { spawn } from "child_process";
import { mkdir } from "fs/promises";

const DEMO_PORT = 9876;
const DEMO_URL = `http://localhost:${DEMO_PORT}`;
const SCREENSHOT_DIR = "tests/screenshots";

let server;

test.beforeAll(async () => {
  await mkdir(SCREENSHOT_DIR, { recursive: true });
  server = spawn("python3", ["-m", "http.server", String(DEMO_PORT)], {
    cwd: new URL("..", import.meta.url).pathname,
    stdio: "pipe",
  });
  // Wait for server to be ready
  for (let i = 0; i < 20; i++) {
    try {
      const res = await fetch(DEMO_URL);
      if (res.ok) return;
    } catch {}
    await new Promise((r) => setTimeout(r, 500));
  }
  throw new Error("Local HTTP server failed to start");
});

test.afterAll(async () => {
  if (server) server.kill();
});

/**
 * Helper: wait for wallets to be initialized (WASM + wallet creation)
 */
async function waitForInit(page) {
  await page.waitForFunction(
    () => {
      const debug = document.getElementById("debug-dump");
      return debug?.textContent?.includes("Wallets initialized");
    },
    { timeout: 60_000 }
  );
}

/**
 * Helper: extract channel state from page via window objects.
 * Returns structured state for both wallets.
 */
async function getChannelState(page) {
  return page.evaluate(() => {
    const getWalletState = (w) => {
      if (!w) return null;
      return {
        pubKeyHex: w.pubKeyHex?.slice(0, 32) + "...",
        channel: w.channel
          ? {
              id: w.channel.id?.slice(0, 16) + "...",
              status: w.channel.status,
              capacity: w.channel.capacity,
              balanceToReceiver: w.channel.balanceToReceiver,
              fundingProofsCount: w.channel.fundingProofs?.length ?? null,
              historyCount: w.channel.history?.length ?? 0,
              history: (w.channel.history || []).map((h) => ({
                phase: h.phase,
                delta: h.delta ?? null,
                balance: h.balance ?? null,
              })),
            }
          : null,
        proofsCount: w.proofs?.length ?? 0,
        proofsTotal: (w.proofs || []).reduce((s, p) => s + p.amount, 0),
        proofs: (w.proofs || []).map((p) => ({
          amount: p.amount,
          secret: p.secret?.slice(0, 16) + "...",
          C: p.C?.slice(0, 16) + "...",
          id: p.id,
        })),
      };
    };
    return {
      alice: getWalletState(window.alice),
      charlie: getWalletState(window.charlie),
    };
  });
}

/**
 * Helper: get DOM-displayed values
 */
async function getDisplayedState(page) {
  return page.evaluate(() => {
    const text = (id) => document.getElementById(id)?.textContent ?? "";
    return {
      alice: {
        pubkey: text("alice-pubkey").slice(0, 40),
        channelId: text("alice-channel-id").slice(0, 40),
        balance: text("alice-balance"),
        proofCount: text("alice-proof-count"),
        proofTotal: text("alice-proof-total"),
        log: text("alice-activity-log"),
      },
      charlie: {
        pubkey: text("charlie-pubkey").slice(0, 40),
        channelId: text("charlie-channel-id").slice(0, 40),
        balance: text("charlie-balance"),
        proofCount: text("charlie-proof-count"),
        proofTotal: text("charlie-proof-total"),
        log: text("charlie-activity-log"),
      },
      debug: text("debug-dump"),
    };
  });
}

/**
 * Helper: screenshot with channel state capture
 */
async function screenshotWithState(page, name) {
  const state = await getChannelState(page);
  const displayed = await getDisplayedState(page);

  await page.screenshot({
    path: `${SCREENSHOT_DIR}/${name}.png`,
    fullPage: true,
  });

  return { state, displayed };
}

// ─── Test Suite ─────────────────────────────────────────────────────

test.describe("Spilman Real Crypto Demo — Full Lifecycle E2E", () => {
  test("page loads with wallets initialized", async ({ page }) => {
    await page.goto(DEMO_URL);
    await page.waitForLoadState("load");
    await waitForInit(page);

    const { state, displayed } = await screenshotWithState(page, "00-initial");

    // Both wallets exist on window
    expect(state.alice).not.toBeNull();
    expect(state.charlie).not.toBeNull();

    // Both have public keys
    expect(state.alice.pubKeyHex).toBeTruthy();
    expect(state.charlie.pubKeyHex).toBeTruthy();

    // No channels yet
    expect(state.alice.channel).toBeNull();
    expect(state.charlie.channel).toBeNull();

    // No proofs
    expect(state.alice.proofsCount).toBe(0);
    expect(state.charlie.proofsCount).toBe(0);

    // Debug panel shows initialization
    expect(displayed.debug).toContain("Wallets initialized");

    // Run button exists and is enabled
    const runBtn = page.getByRole("button", { name: "Run Full Lifecycle" });
    await expect(runBtn).toBeVisible();
    await expect(runBtn).toBeEnabled();
  });

  test("full lifecycle completes successfully", async ({ page }) => {
    await page.goto(DEMO_URL);
    await page.waitForLoadState("load");
    await waitForInit(page);

    // Capture mint network requests
    const mintRequests = [];
    page.on("request", (req) => {
      if (req.url().includes("testnut.cashu.exchange")) {
        mintRequests.push({
          method: req.method(),
          url: req.url(),
          postData: req.postData(),
        });
      }
    });

    const mintResponses = [];
    page.on("response", (res) => {
      if (res.url().includes("testnut.cashu.exchange")) {
        mintResponses.push({
          status: res.status(),
          url: res.url(),
        });
      }
    });

    // Click Run
    await page.getByRole("button", { name: "Run Full Lifecycle" }).click();

    // Wait for lifecycle to complete (max 60s for mint polling)
    await page.waitForFunction(
      () => {
        const debug = document.getElementById("debug-dump");
        return debug?.textContent?.includes("Lifecycle Complete");
      },
      { timeout: 60_000 }
    );

    const { state, displayed } = await screenshotWithState(
      page,
      "01-full-lifecycle"
    );

    // ─── Phase 1: Channel Open ───
    expect(displayed.debug).toContain("Phase 1: Opening channel");
    expect(state.alice.channel).not.toBeNull();
    expect(state.charlie.channel).not.toBeNull();

    // Both wallets computed same channel ID
    expect(state.alice.channel.id).toBe(state.charlie.channel.id);
    expect(state.alice.channel.id).toMatch(/^[0-9a-f]{16}\.\.\.$/);

    // ─── Phase 2: Funding ───
    expect(displayed.debug).toContain("Phase 2: Funding channel");
    expect(displayed.debug).toContain("Channel funded");

    // Alice has refund proofs after cooperative close
    expect(state.alice.proofsCount).toBeGreaterThan(0);
    expect(state.alice.proofsTotal).toBeGreaterThan(0);
    // Refund = fundingTokenAmount - 30 (Charlie) - fee
    expect(state.alice.proofsTotal + 30).toBeLessThanOrEqual(102);

    // Each proof has required fields
    for (const proof of state.alice.proofs) {
      expect(proof.amount).toBeGreaterThan(0);
      expect(proof.secret).toBeTruthy();
      expect(proof.C).toBeTruthy();
      expect(proof.id).toBeTruthy();
    }

    // ─── Phase 3 & 4: Payments ───
    expect(displayed.debug).toContain("Payment 2");

    // Balance to receiver is 30 sat (10 + 20) — proves both payments executed
    expect(state.alice.channel.balanceToReceiver).toBe(30);
    expect(state.charlie.channel.balanceToReceiver).toBe(30);

    // History shows FUNDED + 2x PAYMENT
    const alicePhases = state.alice.channel.history.map((h) => h.phase);
    expect(alicePhases).toContain("FUNDED");
    expect(alicePhases.filter((p) => p === "PAYMENT").length).toBe(2);

    // ─── Phase 5: Cooperative Close ───
    expect(displayed.debug).toContain("Cooperative close");
    expect(displayed.debug).toContain("Channel closed");

    // Charlie received 30 sat
    expect(displayed.debug).toContain("Charlie received: 30 sat");

    // Charlie has proofs from swap
    expect(state.charlie.proofsCount).toBeGreaterThan(0);
    expect(state.charlie.proofsTotal).toBe(30);

    // Alice got refund (dynamic: fundingTokenAmount - Charlie - fee)
    expect(displayed.debug).toContain("Alice refunded:");

    // Charlie's channel is CLOSED
    expect(state.charlie.channel.status).toBe("CLOSED");

    // ─── Mint Network Requests ───
    // Should have: keysets, keys, mint-quote, mint-quote-poll, mint, swap, keysets, keys
    expect(mintResponses.length).toBeGreaterThanOrEqual(8);

    const mintStatuses = mintResponses.map((r) => r.status);
    expect(mintStatuses.every((s) => s === 200)).toBe(true);

    // Verify specific endpoints were called
    const mintUrls = mintRequests.map((r) => r.url);
    expect(mintUrls.some((u) => u.includes("/v1/keysets"))).toBe(true);
    expect(mintUrls.some((u) => u.includes("/v1/keys/"))).toBe(true);
    expect(
      mintUrls.some((u) => u.includes("/v1/mint/quote/bolt11"))
    ).toBe(true);
    expect(mintUrls.some((u) => u.includes("/v1/mint/bolt11"))).toBe(true);
    expect(mintUrls.some((u) => u.includes("/v1/swap"))).toBe(true);
  });

  test("step-by-step lifecycle with state tracking", async ({ page }) => {
    await page.goto(DEMO_URL);
    await page.waitForLoadState("load");
    await waitForInit(page);

    // ─── Step 1: Open Channel ───
    await page.getByRole("button", { name: "Step 1: Open Channel" }).click();
    await page.waitForFunction(
      () =>
        document.getElementById("debug-dump")?.textContent?.includes(
          "Channel opened"
        ),
      { timeout: 30_000 }
    );

    let { state, displayed } = await screenshotWithState(
      page,
      "02-step1-open"
    );

    expect(state.alice.channel).not.toBeNull();
    expect(state.alice.channel.status).toBe("INIT");
    expect(state.alice.channel.capacity).toBe(100);
    expect(state.alice.channel.balanceToReceiver).toBe(0);

    // ─── Step 2: Deposit ───
    await page.getByRole("button", { name: "Step 2: Deposit" }).click();
    await page.waitForFunction(
      () =>
        document.getElementById("debug-dump")?.textContent?.includes(
          "Channel funded"
        ),
      { timeout: 30_000 }
    );

    ({ state, displayed } = await screenshotWithState(page, "03-step2-fund"));

    expect(state.alice.channel.status).toBe("FUNDED");
    expect(state.alice.proofsCount).toBeGreaterThan(0);
    expect(state.alice.proofsTotal).toBeGreaterThan(0);

    // ─── Step 3: Pay ───
    await page.getByRole("button", { name: "Step 3: Pay" }).click();
    await page.waitForFunction(
      () =>
        document.getElementById("debug-dump")?.textContent?.includes(
          "Payment 1"
        ),
      { timeout: 10_000 }
    );

    ({ state, displayed } = await screenshotWithState(page, "04-step3-pay"));

    expect(state.alice.channel.balanceToReceiver).toBe(10);

    // ─── Step 4: Meter (second payment) ───
    await page.getByRole("button", { name: "Step 4: Meter" }).click();
    await page.waitForFunction(
      () =>
        document.getElementById("debug-dump")?.textContent?.includes(
          "Payment 2"
        ),
      { timeout: 10_000 }
    );

    ({ state, displayed } = await screenshotWithState(page, "05-step4-meter"));

    expect(state.alice.channel.balanceToReceiver).toBe(30);

    // ─── Step 5: Close ───
    await page.getByRole("button", { name: "Step 5: Close" }).click();
    await page.waitForFunction(
      () =>
        document.getElementById("debug-dump")?.textContent?.includes(
          "Channel closed"
        ),
      { timeout: 30_000 }
    );

    ({ state, displayed } = await screenshotWithState(page, "06-step5-close"));

    expect(state.charlie.channel.status).toBe("CLOSED");
    expect(state.charlie.proofsTotal).toBe(30);
  });

  test("reset clears state and generates new wallets", async ({ page }) => {
    await page.goto(DEMO_URL);
    await page.waitForLoadState("load");
    await waitForInit(page);

    // Run lifecycle first
    await page.getByRole("button", { name: "Run Full Lifecycle" }).click();
    await page.waitForFunction(
      () =>
        document.getElementById("debug-dump")?.textContent?.includes(
          "Lifecycle Complete"
        ),
      { timeout: 60_000 }
    );

    const beforeReset = await getChannelState(page);
    const oldAliceKey = beforeReset.alice.pubKeyHex;

    // Reset
    await page.getByRole("button", { name: "Reset" }).click();

    const afterReset = await screenshotWithState(page, "07-after-reset");

    // New wallets generated (different keys)
    expect(afterReset.state.alice.pubKeyHex).not.toBe(oldAliceKey);

    // Channel is null again
    expect(afterReset.state.alice.channel).toBeNull();
    expect(afterReset.state.charlie.channel).toBeNull();

    // No proofs
    expect(afterReset.state.alice.proofsCount).toBe(0);
    expect(afterReset.state.charlie.proofsCount).toBe(0);
  });

  test("proof structure is valid after funding", async ({ page }) => {
    await page.goto(DEMO_URL);
    await page.waitForLoadState("load");
    await waitForInit(page);

    await page.getByRole("button", { name: "Run Full Lifecycle" }).click();
    await page.waitForFunction(
      () =>
        document.getElementById("debug-dump")?.textContent?.includes(
          "Lifecycle Complete"
        ),
      { timeout: 60_000 }
    );

    const state = await getChannelState(page);

    // Charlie's proofs are valid
    for (const proof of state.charlie.proofs) {
      // Amount is a power of 2 (Cashu denomination)
      expect(proof.amount).toBeGreaterThan(0);
      expect(Number.isInteger(Math.log2(proof.amount))).toBe(true);

      // Secret is hex string
      expect(proof.secret).toMatch(/^[0-9a-f]{16}\.\.\.$/);

      // C is a hex point
      expect(proof.C).toMatch(/^[0-9a-f]{16}\.\.\.$/);

      // ID matches keyset
      expect(proof.id).toBeTruthy();
    }

    // Total proofs equal Charlie's balance
    const total = state.charlie.proofs.reduce((s, p) => s + p.amount, 0);
    expect(total).toBe(30);
  });

  test("debug panel shows all phases in order", async ({ page }) => {
    await page.goto(DEMO_URL);
    await page.waitForLoadState("load");
    await waitForInit(page);

    await page.getByRole("button", { name: "Run Full Lifecycle" }).click();
    await page.waitForFunction(
      () =>
        document.getElementById("debug-dump")?.textContent?.includes(
          "Lifecycle Complete"
        ),
      { timeout: 60_000 }
    );

    const displayed = await getDisplayedState(page);
    const debug = displayed.debug;

    // Phases appear in order
    const phases = [
      "Wallets initialized",
      "Starting Full Lifecycle",
      "Opening channel",
      "Channel opened",
      "Funding channel",
      "Channel funded",
      "Payment 2",
      "Cooperative close",
      "Channel closed",
      "Lifecycle Complete",
      "Charlie received: 30 sat",
      "Alice refunded:",
    ];

    for (const phase of phases) {
      expect(debug).toContain(phase);
    }

    const indices = phases.map((p) => debug.indexOf(p));
    for (let i = 1; i < indices.length; i++) {
      expect(indices[i]).toBeGreaterThan(indices[i - 1]);
    }
  });
});

# In-Browser Cashu Spilman Channel Demo (Option B)

## TL;DR

> **Quick Summary**: Build a multi-file ESM browser demo at `docs/private/demos/spilman-real/` that runs Alice (buyer) and Charlie (seller) Cashu wallets in the same page, performing a full Spilman channel lifecycle (ECDH → funding → payments → cooperative close) against the real testnut.cashu.exchange mint. Mirrors `crates/tollgate-net/tests/cdk_spilman_bridge_spike.rs` 1:1, in JavaScript.
>
> **Deliverables**:
> - `docs/private/demos/spilman-real/` — multi-file ESM project (HTML + JS modules + CSS), no build step
> - JS reimplementation of 8 cdk-spilman crypto functions (ECDH, channel ID, funding outputs, construct proofs, verify, signed balance update, etc.)
> - Real interaction with testnut.cashu.exchange (mint quotes, mint proofs, swap)
> - Split-screen dark-themed UI with step-by-step walkthrough
> - Deployed to GitHub Pages sub-path: `/spilman-real/`
> - Test vectors captured from Rust spike for crypto validation
> - README explaining architecture, what's real vs simulated
>
> **Estimated Effort**: Large
> **Parallel Execution**: YES — 4 waves
> **Critical Path**: T1 → T2 (test vectors) → T6 (funding outputs) → T7 (construct/verify) → T8 (balance update) → T11 (cooperative close) → F1-F4

---

## Context

### Original Request
"i want you to restart planning for option b so that we get a full demo in browser showing spilman cashu channels with either cashu-ts or https://github.com/cashubtc/coco. the end goal will probably be to include spilman cashu channels into cashu-ts or coco but for our first proof of concept just having a fully working demo with a buyer and a seller that can interact in the browser and actually running against a real mint would be a good start"

### Interview Summary

**Library Decision**: Use cashu-ts directly. coco-cashu-core depends on `@cashu/cashu-ts: 2.9.0` — it's a higher-level toolkit (storage adapters, React hooks, plugin system) built ON cashu-ts. Our PoC needs none of those features. cashu-ts 4.2.1 loads from esm.sh and pulls `@noble/curves` (secp256k1, ECDH, Schnorr/BIP-340), `@noble/hashes` (SHA-256), `@scure/bip32` transitively — exactly the primitives we need. If we want to upstream Spilman later, contributing to cashu-ts puts us at the protocol layer; coco can adopt for free.

**Confirmed user choices**:
- UI: Single page, split-screen (Alice left, Charlie right), dark theme matching existing simulator
- Crypto realism: Full fidelity — real ECDH, real Schnorr, real DLEQ, real mint
- Mint: testnut.cashu.exchange (public, FakeWallet auto-pays Lightning)
- Project structure: Multi-file ESM under `docs/private/demos/spilman-real/`
- Wallet communication: Direct in-page function calls (no postMessage, no iframes, no fake network)
- Persistence: In-memory only, no localStorage
- Refund/timeout: Skip entirely — cooperative close only
- Parity: 1:1 mirror of `cdk_spilman_bridge_spike.rs`
- Testing: Agent-executed Playwright QA only, no unit test framework
- Deployment: GitHub Pages sub-path under existing site
- Upstream strategy: Build demo first, extract reusable lib later

### Research Findings

- **CORS confirmed**: testnut.cashu.exchange returns `access-control-allow-origin: *` — no proxy needed for browser
- **Active keyset**: `008e808b89acc141` with `input_fee_ppk=10` (fees must be accounted for, not zero)
- **cashu-ts modules available via esm.sh**: NUT01 (blinding), NUT11 (P2PK), NUT12 (DLEQ), NUT13 (deterministic secrets), NUT20 (signed quotes)
- **No existing JS Spilman implementation exists** — we're reimplementing cdk-spilman crypto from scratch in JS
- **8 cdk-spilman wrapper functions to reimplement**: `compute_channel_secret_from_hex`, `compute_funding_token_amount`, `channel_parameters_get_channel_id`, `create_funding_outputs`, `create_signed_balance_update`, `construct_proofs`, `verify_valid_channel`, `parse_keyset_info_from_json`
- **Reference Rust source**: cdk-spilman from SatsAndSports fork (referenced in our `Cargo.toml`)
- **Blinding derivation**: `r = SHA256("Cashu_Spilman_P2BK_v1" || channel_secret || "{channel_id}|{context}|{retry_counter}")` — MUST be byte-verified against Rust source

### Metis Review

**Critical insight**: Front-load crypto risk. The hardest milestone is unblinding (constructing spendable proofs from blind signatures). UI is easy; crypto byte-level fidelity is hard.

**Identified Gaps (addressed in plan)**:
- No JS reference for cdk-spilman → Plan includes early task to read Rust source and capture test vectors
- cashu-ts API surface uncertain → Early validation milestone before committing to it for blinding/unblinding
- Schnorr key tweaking conventions → Read Rust source, document tweak formula
- input_fee_ppk handling → Fetched dynamically, displayed in UI
- Crypto debug visibility → All hex intermediates logged to console + optional UI panel

---

## Work Objectives

### Core Objective
Demonstrate a complete Cashu Spilman channel lifecycle in the browser between two wallets running in the same page, against a real Cashu mint, using real crypto. This is the integration test, in JavaScript, visible.

### Concrete Deliverables
- `docs/private/demos/spilman-real/index.html` — entry point with split-screen UI shell
- `docs/private/demos/spilman-real/style.css` — dark theme matching existing simulator
- `docs/private/demos/spilman-real/src/crypto.js` — JS implementations of cdk-spilman crypto primitives
- `docs/private/demos/spilman-real/src/mint.js` — raw fetch() wrappers for testnut HTTP API
- `docs/private/demos/spilman-real/src/channel.js` — Spilman channel state machine
- `docs/private/demos/spilman-real/src/wallet.js` — Alice + Charlie wallet objects
- `docs/private/demos/spilman-real/src/ui.js` — DOM updates, step navigation, debug panel
- `docs/private/demos/spilman-real/src/main.js` — entry point, wires everything together
- `docs/private/demos/spilman-real/test-vectors.json` — captured intermediate values from Rust spike
- `docs/private/demos/spilman-real/README.md` — architecture, what's real vs simplified, how to run
- `.github/workflows/ci.yml` updated to deploy `/spilman-real/` sub-path
- Live deployment at `https://amperstrand.github.io/tollgate-rs-ai-research-and-experiments/spilman-real/`

### Definition of Done
- [ ] Visiting `https://amperstrand.github.io/.../spilman-real/` loads the page with no console errors
- [ ] Clicking "Run Full Lifecycle" executes all 8 phases against real testnut mint
- [ ] Both Alice and Charlie panels show updated state at each phase
- [ ] Final state: cooperative close completes, Charlie has ≥50 sat in proofs, Alice has refund proofs
- [ ] Playwright scenario navigates the page and asserts on DOM content + console output
- [ ] Lifecycle completes in <60 seconds (within mint timeout limits)
- [ ] All crypto intermediates (channel_secret, channel_id, blinding factors, signatures) match Rust spike test vectors

### Must Have
- Real ECDH using secp256k1 (`@noble/curves/secp256k1.getSharedSecret`)
- Real BIP-340 Schnorr signatures
- Real mint interaction (no mocked HTTP)
- Real DLEQ verification of mint signatures
- Deterministic blinding matching Rust formula byte-for-byte
- 1:1 phase ordering with `cdk_spilman_bridge_spike.rs`
- Multi-file ESM project structure (no single 5000-line HTML file)
- Direct in-page function calls between Alice and Charlie objects
- Test vectors captured from Rust spike test
- Debug console logs for every crypto operation showing hex intermediates
- Reset button to restart from clean state (in-memory only)

### Must NOT Have (Guardrails)
- ❌ NO refund/timeout close path — cooperative close only
- ❌ NO localStorage / IndexedDB / any persistence — in-memory only
- ❌ NO unit test framework — Playwright agent-executed QA only
- ❌ NO build step (no webpack/vite/esbuild/tsdown) — ESM imports from esm.sh only
- ❌ NO Coco integration — cashu-ts only
- ❌ NO postMessage / iframes / web workers — direct function calls
- ❌ NO multi-channel — one channel per demo run
- ❌ NO rollover — single funded channel
- ❌ NO bidirectional payments — Alice → Charlie only
- ❌ NO mobile responsiveness — desktop-only PoC
- ❌ NO library extraction during this plan — inline code first, extract later
- ❌ NO custom Error class hierarchy — plain `Error()` with descriptive messages
- ❌ NO TypeScript — plain JS with JSDoc only where it aids debugging
- ❌ NO React/Vue/framework — vanilla DOM
- ❌ NO npm install / package.json — pure browser-loaded ESM
- ❌ NO production error recovery — fail fast with clear errors
- ❌ NO authentication / signing on inter-wallet messages — direct calls trusted
- ❌ NO concurrent operations — strict sequential lifecycle
- ❌ NO premature abstraction (`SpilmanChannel` class supporting both directions, etc.)
- ❌ NO key management — generate ephemeral keys per page load, display in UI
- ❌ NO upstream PR work in this plan — separate effort

---

## Verification Strategy

> **ZERO HUMAN INTERVENTION** — ALL verification is agent-executed via Playwright. No "user manually tests" criteria.

### Test Decision
- **Infrastructure exists**: NO (no JS test framework in repo today)
- **Automated tests**: NONE (Playwright QA only, per user choice)
- **Framework**: None for unit tests; Playwright for end-to-end
- **Approach**: Each task ships with Playwright scenarios that drive the browser, perform interactions, assert on DOM + console, and capture screenshots/network logs as evidence

### QA Policy
Every task MUST include agent-executed QA scenarios using Playwright (`playwright_browser_*` tools). Evidence saved to `.sisyphus/evidence/task-{N}-{scenario}.{ext}`.

- **Page loads**: `playwright_browser_navigate` → `playwright_browser_snapshot` → `playwright_browser_console_messages level=error` (assert empty)
- **Crypto operations**: Trigger via UI button → assert on hex output displayed in DOM AND logged to console
- **Mint interaction**: Trigger via UI → `playwright_browser_network_requests filter="testnut"` → assert request/response shape
- **Test vector validation**: Page exposes `window.runTestVectors()` → returns `{ pass: true/false, mismatches: [...] }` → assert pass=true
- **Full lifecycle**: Single button triggers all 8 phases → assert final state in both panels + console shows no errors

### Test Vector Strategy (CRITICAL — Risk Mitigation)
- T2 captures test vectors by running the Rust spike test with extra logging
- Vectors saved to `test-vectors.json`: channel_secret, channel_id, funding amounts, blinded message hex, signature hex
- Each crypto task runs vectors via `window.runTestVectors()` and asserts JS output matches Rust output byte-for-byte
- Without this, JS crypto bugs are nearly undebuggable

---

## Execution Strategy

### Parallel Execution Waves

```
Wave 1 (Start Immediately — foundation, no dependencies):
├── T1: Project scaffolding + index.html + CSS dark theme [quick]
├── T2: Capture Rust test vectors [deep]  (Rust work, parallel to UI work)
├── T3: cashu-ts ESM load probe + API surface validation [quick]
└── T4: Mint HTTP wrappers (mint.js — pure fetch) [quick]

Wave 2 (After Wave 1 — crypto primitives, parallel):
├── T5: ECDH + channel ID (crypto.js part 1) [deep]  (deps: T2, T3)
├── T6: Deterministic blinding + funding outputs [deep]  (deps: T2, T3, T5)
├── T7: Construct proofs from blind sigs + verify_valid_channel [deep]  (deps: T2, T3, T6)
└── T8: Schnorr-tweaked signed balance update [deep]  (deps: T2, T3, T5)

Wave 3 (After Wave 2 — channel state machine + UI):
├── T9: Channel state machine (channel.js) [unspecified-high]  (deps: T5, T6, T7, T8)
├── T10: Wallet objects (wallet.js) [unspecified-high]  (deps: T4, T9)
├── T11: Cooperative close via mint swap [deep]  (deps: T7, T9, T10)
├── T12: UI step-by-step + debug panel + reset (ui.js) [visual-engineering]  (deps: T1, T9)
└── T13: Main entry point + wiring (main.js) [quick]  (deps: T9, T10, T11, T12)

Wave 4 (After Wave 3 — integration + deploy):
├── T14: README.md + architecture documentation [writing]  (deps: T13)
├── T15: GitHub Pages CI deploy sub-path [quick]  (deps: T13)
└── T16: End-to-end Playwright lifecycle test [deep]  (deps: T13, T15)

Wave FINAL (After ALL — 4 parallel reviews, then user okay):
├── F1: Plan compliance audit (oracle)
├── F2: Code quality review (unspecified-high)
├── F3: Real manual QA — full lifecycle browser run (unspecified-high + playwright)
└── F4: Scope fidelity check (deep)
→ Present results → Get explicit user okay

Critical Path: T2 → T6 → T7 → T8 → T11 → T13 → T16 → F1-F4 → user okay
Parallel Speedup: ~60% faster than sequential
Max Concurrent: 5 (Wave 3)
```

### Dependency Matrix

| Task | Depends On | Blocks |
|------|-----------|--------|
| T1 | — | T12 |
| T2 | — | T5, T6, T7, T8 |
| T3 | — | T5, T6, T7, T8 |
| T4 | — | T10 |
| T5 | T2, T3 | T6, T8, T9 |
| T6 | T2, T3, T5 | T7, T9 |
| T7 | T2, T3, T6 | T9, T11 |
| T8 | T2, T3, T5 | T9 |
| T9 | T5, T6, T7, T8 | T10, T11, T12, T13 |
| T10 | T4, T9 | T11, T13 |
| T11 | T7, T9, T10 | T13 |
| T12 | T1, T9 | T13 |
| T13 | T9, T10, T11, T12 | T14, T15, T16 |
| T14 | T13 | F1 |
| T15 | T13 | T16, F3 |
| T16 | T13, T15 | F1-F4 |

### Agent Dispatch Summary

| Wave | Tasks | Agents |
|------|-------|--------|
| 1 | 4 | T1 → `quick`, T2 → `deep`, T3 → `quick`, T4 → `quick` |
| 2 | 4 | T5-T8 → all `deep` (crypto risk) |
| 3 | 5 | T9 → `unspecified-high`, T10 → `unspecified-high`, T11 → `deep`, T12 → `visual-engineering`, T13 → `quick` |
| 4 | 3 | T14 → `writing`, T15 → `quick`, T16 → `deep` |
| FINAL | 4 | F1 → `oracle`, F2 → `unspecified-high`, F3 → `unspecified-high`+`playwright`, F4 → `deep` |

---

## TODOs

- [x] 1. Project scaffolding + index.html + CSS dark theme

  **What to do**:
  - Create directory `docs/private/demos/spilman-real/` with subdirectory `src/`
  - Create `index.html`: ESM entry, `<script type="module" src="./src/main.js">`, viewport meta, charset utf-8, page title "Cashu Spilman Channel — Real Crypto Demo"
  - Layout: header with title + mint URL display + reset button; main split into `<section id="alice">` and `<section id="charlie">`; below them `<section id="lifecycle-controls">` with "Run Full Lifecycle" + step-by-step buttons; `<section id="debug-panel">` for hex dumps
  - Each wallet section has subsections: Identity (pubkey hex), Channel State (id, balance), Proofs (count + total), Activity Log
  - Create `style.css` with dark theme: bg `#0d1117`, text `#e6edf3`, accent `#58a6ff`, panels `#161b22` with `#30363d` borders; monospace for hex values; two-column flex grid for Alice/Charlie split
  - Create empty placeholder `src/main.js` with `console.log("spilman-real loaded")` so page loads without errors
  - Match visual language of `docs/private/demos/spilman-simulator.html` (color palette, font choices)

  **Must NOT do**:
  - NO build step, NO npm install, NO TypeScript, NO framework imports
  - NO mobile media queries
  - NO localStorage references anywhere

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Pure scaffolding — directory creation, static HTML/CSS, no logic. Trivial single-pass work.
  - **Skills**: `[]`
    - No skills needed; file creation only

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1 (with T2, T3, T4)
  - **Blocks**: T12
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `docs/private/demos/spilman-simulator.html:1-200` - Color palette, font stack, panel styling to mirror
  - `docs/private/pages-template/index.html` - GitHub Pages template structure

  **WHY Each Reference Matters**:
  - simulator.html establishes the visual identity our demo must match for consistency
  - pages-template defines how files under `docs/private/demos/` are deployed by CI

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Page loads without errors
    Tool: Playwright
    Preconditions: Local file path or `python3 -m http.server` in repo root
    Steps:
      1. playwright_browser_navigate url="file:///Users/macbook/src/tollgate-rs/docs/private/demos/spilman-real/index.html"
      2. playwright_browser_console_messages level="error"
      3. playwright_browser_snapshot
    Expected Result: Console error count = 0; snapshot shows header, two sections (alice, charlie), lifecycle-controls, debug-panel
    Failure Indicators: Any 404 in network requests; missing DOM sections
    Evidence: .sisyphus/evidence/task-1-page-loads.txt

  Scenario: Dark theme applied
    Tool: Playwright
    Preconditions: Page loaded
    Steps:
      1. playwright_browser_evaluate function="() => getComputedStyle(document.body).backgroundColor"
    Expected Result: rgb value matching `#0d1117` (rgb(13, 17, 23))
    Evidence: .sisyphus/evidence/task-1-dark-theme.txt
  ```

  **Evidence to Capture**:
  - [ ] task-1-page-loads.txt (console + snapshot)
  - [ ] task-1-dark-theme.txt (computed style)

  **Commit**: NO (groups with Wave 1 commit)

- [x] 2. Capture Rust test vectors

  **What to do**:
  - Add a new test `cdk_spilman_bridge_spike_capture_vectors` in `crates/tollgate-net/tests/cdk_spilman_bridge_spike.rs` (or sibling file `cdk_spilman_test_vectors.rs`) that mirrors the spike but writes intermediate values to `docs/private/demos/spilman-real/test-vectors.json`
  - Capture for one full lifecycle: alice_seed_hex, alice_pubkey_hex, charlie_seed_hex, charlie_pubkey_hex, ecdh_shared_secret_hex, channel_secret_hex, channel_id_hex, keyset_id, keyset_input_fee_ppk, funding_amount_sat, funding_blinded_messages (array of {amount, B_, blinding_factor_r}), funding_blind_signatures (array of {amount, C_}), constructed_proofs (array of {amount, secret, C}), one signed_balance_update {amount_to_charlie, signature_hex, message_hash_hex}
  - Use `serde_json::to_string_pretty` for stable diff-friendly output
  - Run via `cargo test -p tollgate-net --test cdk_spilman_test_vectors -- --nocapture` and verify file is written
  - Add `#[ignore]` or env-flag gate so CI doesn't depend on a live mint for this capture; document trigger command in README task (T14)
  - Use deterministic seeds (hex literals in test) so vectors are reproducible

  **Must NOT do**:
  - NO modification of cdk-spilman crate (we depend on the SatsAndSports fork — read-only)
  - NO new public APIs in tollgate-net just for capture; test-only helpers stay in tests/
  - NO leaking real funded wallets — testnut only

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Requires reading cdk-spilman source carefully, instrumenting Rust test, ensuring byte-fidelity with Rust types. High-risk crypto plumbing.
  - **Skills**: `[]`
    - No skill matches; pure Rust + crypto reasoning

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1
  - **Blocks**: T5, T6, T7, T8 (all crypto tasks consume vectors)
  - **Blocked By**: None

  **References**:

  **Pattern References**:
  - `crates/tollgate-net/tests/cdk_spilman_bridge_spike.rs` - 1:1 source spike to mirror
  - `crates/tollgate-net/tests/spilman_integration.rs` - Full lifecycle reference for ordering
  - `crates/tollgate-net/src/spilman_wallet.rs:fetch_active_keyset_info` - Keyset fetch pattern

  **API References**:
  - `cdk-spilman` exports: `compute_channel_secret_from_hex`, `compute_funding_token_amount`, `channel_parameters_get_channel_id`, `create_funding_outputs`, `construct_proofs`, `verify_valid_channel`, `create_signed_balance_update`, `parse_keyset_info_from_json` — these 8 produce the values to capture

  **WHY Each Reference Matters**:
  - The spike test is the executable specification. Any deviation breaks JS parity.
  - cdk-spilman is the source of truth for byte layout; we capture its outputs to assert against in JS.

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Capture vectors produces valid JSON
    Tool: Bash
    Preconditions: testnut.cashu.exchange reachable; Rust toolchain installed
    Steps:
      1. cargo test -p tollgate-net --test cdk_spilman_test_vectors capture_vectors -- --ignored --nocapture
      2. test -f docs/private/demos/spilman-real/test-vectors.json
      3. jq -e '.channel_id_hex and .channel_secret_hex and (.funding_blinded_messages | length > 0)' docs/private/demos/spilman-real/test-vectors.json
    Expected Result: Test passes; file exists; jq returns true (all required keys present and non-empty arrays)
    Failure Indicators: Missing keys; empty arrays; non-hex strings in *_hex fields
    Evidence: .sisyphus/evidence/task-2-vectors-captured.json (copy of file)

  Scenario: Vectors are deterministic across runs
    Tool: Bash
    Preconditions: Vectors captured once
    Steps:
      1. cp docs/private/demos/spilman-real/test-vectors.json /tmp/vectors-1.json
      2. cargo test -p tollgate-net --test cdk_spilman_test_vectors capture_vectors -- --ignored --nocapture
      3. diff <(jq -S '.channel_id_hex, .channel_secret_hex, .ecdh_shared_secret_hex' /tmp/vectors-1.json) <(jq -S '.channel_id_hex, .channel_secret_hex, .ecdh_shared_secret_hex' docs/private/demos/spilman-real/test-vectors.json)
    Expected Result: diff exit 0 (identical) for deterministic fields
    Failure Indicators: Diff non-empty (non-determinism in capture)
    Evidence: .sisyphus/evidence/task-2-deterministic.txt
  ```

  **Evidence to Capture**:
  - [ ] task-2-vectors-captured.json
  - [ ] task-2-deterministic.txt

  **Commit**: NO (groups with Wave 1 commit)

- [x] 3. cashu-ts ESM load probe + API surface validation

  **What to do**:
  - Create `docs/private/demos/spilman-real/src/probe.html` (temporary, deleted in T13): minimal HTML that imports `https://esm.sh/@cashu/cashu-ts@4.2.1`, `https://esm.sh/@noble/curves@2.2.0/secp256k1`, `https://esm.sh/@noble/hashes@2.2.0/sha2`, `https://esm.sh/@noble/hashes@2.2.0/utils`
  - In `<script type="module">`: log all named exports of cashu-ts (`Object.keys(cashuModule)`); log secp256k1 utility functions; verify `getSharedSecret`, `schnorr.sign`, `schnorr.verify`, `sha256`, `bytesToHex`, `hexToBytes` exist
  - Expose `window.probeResult = { cashuTsExports: [...], hasGetSharedSecret: bool, hasSchnorr: bool, ... }` for Playwright assertion
  - Document findings in inline comment at top of `src/crypto.js` skeleton (created in this task as empty file with import block)
  - Confirm cashu-ts gives us: `CashuMint`, `CashuWallet`, `getKeysApi` or equivalent for keyset fetch; if `BlindedMessage`/`BlindSignature` types are exported, note them — otherwise we'll roll our own

  **Must NOT do**:
  - NO importing cashu-ts internals via deep paths (only top-level `@cashu/cashu-ts` and submodules `/crypto`, `/secrets` if documented public)
  - NO writing actual crypto yet — this is a discovery task

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Probe-and-document. Browser-load and inspect; minimal logic.
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1
  - **Blocks**: T5, T6, T7, T8
  - **Blocked By**: None

  **References**:

  **External References**:
  - `https://esm.sh/@cashu/cashu-ts@4.2.1` - ESM bundle URL
  - `https://github.com/cashubtc/cashu-ts` - Source for API names
  - `https://github.com/paulmillr/noble-curves` - secp256k1 + schnorr docs

  **WHY**:
  - Confirms our import URLs work and named exports match expectations before crypto tasks depend on them
  - Catches CDN/version surprises early

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: All required exports present
    Tool: Playwright
    Preconditions: probe.html exists
    Steps:
      1. playwright_browser_navigate url="file:///Users/macbook/src/tollgate-rs/docs/private/demos/spilman-real/src/probe.html"
      2. playwright_browser_wait_for time=3
      3. playwright_browser_evaluate function="() => window.probeResult"
    Expected Result: { hasGetSharedSecret: true, hasSchnorr: true, hasSha256: true, cashuTsExports.length > 5 }
    Failure Indicators: Any `has*: false`; console errors about module resolution
    Evidence: .sisyphus/evidence/task-3-probe-result.json

  Scenario: No CORS errors loading from esm.sh
    Tool: Playwright
    Preconditions: Page loaded
    Steps:
      1. playwright_browser_console_messages level="error"
      2. playwright_browser_network_requests filter="esm.sh"
    Expected Result: Zero errors; all esm.sh requests status 200
    Evidence: .sisyphus/evidence/task-3-network.txt
  ```

  **Evidence to Capture**:
  - [ ] task-3-probe-result.json
  - [ ] task-3-network.txt

  **Commit**: NO (groups with Wave 1 commit)

- [x] 4. Mint HTTP wrappers (mint.js — pure fetch)

  **What to do**:
  - Create `docs/private/demos/spilman-real/src/mint.js` exporting:
    - `MINT_URL = "https://testnut.cashu.exchange"`
    - `async function getKeysets()` → GET `/v1/keysets` → returns parsed JSON `{ keysets: [...] }`
    - `async function getKeys(keysetId)` → GET `/v1/keys/{keysetId}` → returns `{ keysets: [{ id, unit, keys: { "1": "02ab..", "2": "03cd.." } }] }`
    - `async function getMintInfo()` → GET `/v1/info`
    - `async function postMintQuoteBolt11(amountSat)` → POST `/v1/mint/quote/bolt11` body `{ unit: "sat", amount }` → `{ quote, request, paid, expiry }`
    - `async function getMintQuoteState(quoteId)` → GET `/v1/mint/quote/bolt11/{quote}` → state polling
    - `async function postMintBolt11({ quote, outputs })` → POST `/v1/mint/bolt11` → `{ signatures: [...] }`
    - `async function postSwap({ inputs, outputs })` → POST `/v1/swap` → `{ signatures: [...] }`
    - `async function postCheckState({ Ys })` → POST `/v1/checkstate` → spent/unspent status
  - Each function uses `fetch()` with `headers: { "content-type": "application/json", "accept": "application/json" }`; throws `new Error(...)` with status + response body on non-2xx
  - Add `pollMintQuote(quoteId, { intervalMs = 1500, timeoutMs = 30000 })` helper that polls until `paid: true` or timeout
  - All functions accept optional `mintUrl` first arg for testability, defaulting to MINT_URL

  **Must NOT do**:
  - NO retry logic with exponential backoff — fail fast
  - NO request signing (NUT-20) yet
  - NO custom error classes — just `new Error("mint POST /v1/swap failed: 400 ...")`
  - NO use of cashu-ts CashuMint client — we want raw HTTP visibility

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Thin wrappers around fetch. No crypto.
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 1
  - **Blocks**: T10
  - **Blocked By**: None

  **References**:

  **External References**:
  - `https://github.com/cashubtc/nuts` - NUT specs (NUT-01 keys, NUT-02 keysets, NUT-04 mint, NUT-03 swap, NUT-07 checkstate)
  - `https://testnut.cashu.exchange/v1/info` - Live response for shape verification

  **Pattern References**:
  - `crates/tollgate-net/src/spilman_wallet.rs:fetch_active_keyset_info` - How Rust does the same calls; mirror parameter names

  **WHY**:
  - NUT specs define exact request/response shapes; deviating means runtime errors against real mint
  - Rust wrapper proves ordering and parameter conventions

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: getKeysets returns active keyset
    Tool: Playwright
    Preconditions: probe.html or test page imports mint.js and calls getKeysets()
    Steps:
      1. playwright_browser_navigate url="file:///.../spilman-real/src/probe.html"
      2. playwright_browser_evaluate function="async () => { const m = await import('./mint.js'); return await m.getKeysets(); }"
    Expected Result: Object with `keysets` array containing entry where `id == "008e808b89acc141"` and `input_fee_ppk == 10`
    Failure Indicators: Network error; missing fields; wrong active keyset id (mint may rotate — update vector if so)
    Evidence: .sisyphus/evidence/task-4-keysets.json

  Scenario: postMintQuoteBolt11 returns invoice
    Tool: Playwright
    Preconditions: Page loaded, mint.js importable
    Steps:
      1. playwright_browser_evaluate function="async () => { const m = await import('./mint.js'); return await m.postMintQuoteBolt11(10); }"
    Expected Result: Object with non-empty `quote` (string) and `request` (lnbc... bolt11 invoice string)
    Evidence: .sisyphus/evidence/task-4-mint-quote.json

  Scenario: Errors include status + body
    Tool: Playwright
    Preconditions: Page loaded
    Steps:
      1. playwright_browser_evaluate function="async () => { const m = await import('./mint.js'); try { await m.getKeys('not-a-real-keyset-id'); return 'no-throw'; } catch(e) { return e.message; } }"
    Expected Result: String containing "404" or HTTP error code AND endpoint path
    Evidence: .sisyphus/evidence/task-4-error-shape.txt
  ```

  **Evidence to Capture**:
  - [ ] task-4-keysets.json
  - [ ] task-4-mint-quote.json
  - [ ] task-4-error-shape.txt

  **Commit**: YES (Wave 1 commit groups T1-T4)
  - Message: `feat(demo): scaffold spilman-real browser demo + capture test vectors`
  - Files: `docs/private/demos/spilman-real/index.html`, `docs/private/demos/spilman-real/style.css`, `docs/private/demos/spilman-real/src/main.js`, `docs/private/demos/spilman-real/src/mint.js`, `docs/private/demos/spilman-real/src/probe.html`, `docs/private/demos/spilman-real/test-vectors.json`, `crates/tollgate-net/tests/cdk_spilman_test_vectors.rs`
  - Pre-commit: `cargo test -p tollgate-net --test cdk_spilman_test_vectors capture_vectors -- --ignored` and Playwright probe scenario

- [x] 5. ECDH + channel ID (crypto.js part 1)

  **What to do**:
  - Create `docs/private/demos/spilman-real/src/crypto.js`; in it implement:
    - `generateKeypair()` → `{ privHex, pubHex }` using `secp256k1.utils.randomPrivateKey()` + `secp256k1.getPublicKey(priv, true)` (compressed)
    - `computeEcdhSharedSecret(myPrivHex, theirPubHex)` → uses `secp256k1.getSharedSecret(priv, pub)` returning compressed shared point; matches Rust `cdk-spilman` ECDH convention exactly (verify against Rust source: which serialization, which hash if any)
    - `computeChannelSecretFromHex(sharedSecretHex)` → mirror of `compute_channel_secret_from_hex` (likely SHA256 with domain tag — confirm in Rust source)
    - `computeChannelId(channelParams)` → mirror of `channel_parameters_get_channel_id` — accepts struct `{ alicePubHex, charliePubHex, keysetId, ... }`, produces deterministic id
  - Each function has a JSDoc comment with `// Rust ref: cdk-spilman::compute_channel_secret_from_hex` pointing to the source function
  - Add `window.runVectors = async () => { ... }` (later expanded by T6/T7/T8) that loads `test-vectors.json` via fetch, runs each function with vector inputs, asserts output hex matches; returns `{ pass: bool, mismatches: [...] }`
  - Use `bytesToHex` / `hexToBytes` from `@noble/hashes/utils`

  **Must NOT do**:
  - NO custom hash implementations — use `@noble/hashes`
  - NO sync XHR — async only
  - NO assumptions about endianness without checking Rust source

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Byte-exact crypto parity is the highest-risk task. Must read Rust source carefully and verify with vectors. Single mistake = silent broken channels.
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2 (with T6, T7, T8)
  - **Blocks**: T6, T8, T9
  - **Blocked By**: T2 (vectors), T3 (cashu-ts probe)

  **References**:

  **Pattern References**:
  - cdk-spilman crate sources (vendored via Cargo.toml git dep): `compute_channel_secret_from_hex`, `channel_parameters_get_channel_id` — must read exact byte layout
  - `crates/tollgate-net/tests/cdk_spilman_bridge_spike.rs` - shows call ordering

  **External References**:
  - `https://github.com/paulmillr/noble-curves#secp256k1` - `getSharedSecret`, `getPublicKey` API
  - `https://github.com/paulmillr/noble-hashes#sha256` - `sha256` and `hmac` if needed

  **WHY**:
  - Byte parity = correctness. Vectors from T2 are the ONLY reliable test.

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: ECDH + channel ID match Rust vectors
    Tool: Playwright
    Preconditions: test-vectors.json present; crypto.js loaded
    Steps:
      1. playwright_browser_navigate url=".../spilman-real/index.html"
      2. playwright_browser_evaluate function="async () => { const c = await import('./src/crypto.js'); const v = await fetch('./test-vectors.json').then(r=>r.json()); const ecdh = c.computeEcdhSharedSecret(v.alice_seed_hex, v.charlie_pubkey_hex); const cs = c.computeChannelSecretFromHex(ecdh); const cid = c.computeChannelId({alicePubHex: v.alice_pubkey_hex, charliePubHex: v.charlie_pubkey_hex, keysetId: v.keyset_id}); return { ecdhMatch: ecdh === v.ecdh_shared_secret_hex, csMatch: cs === v.channel_secret_hex, cidMatch: cid === v.channel_id_hex }; }"
    Expected Result: { ecdhMatch: true, csMatch: true, cidMatch: true }
    Failure Indicators: Any field false → bytewise mismatch with Rust; check serialization/hash domain
    Evidence: .sisyphus/evidence/task-5-vector-match.json

  Scenario: Generate fresh keypair produces valid secp256k1
    Tool: Playwright
    Steps:
      1. playwright_browser_evaluate function="async () => { const c = await import('./src/crypto.js'); const k = c.generateKeypair(); return { privLen: k.privHex.length, pubLen: k.pubHex.length, pubPrefix: k.pubHex.slice(0,2) }; }"
    Expected Result: { privLen: 64, pubLen: 66, pubPrefix: "02" or "03" } (compressed)
    Evidence: .sisyphus/evidence/task-5-keypair.json
  ```

  **Evidence to Capture**:
  - [ ] task-5-vector-match.json
  - [ ] task-5-keypair.json

  **Commit**: NO (groups with Wave 2 commit)

- [x] 6. Deterministic blinding + funding outputs

  **What to do**:
  - Extend `src/crypto.js` with:
    - `deriveBlindingFactor({ channelSecretHex, channelIdHex, context, retryCounter = 0 })` → `r` scalar; formula: `r = SHA256("Cashu_Spilman_P2BK_v1" || channel_secret_bytes || ascii("{channel_id}|{context}|{retry_counter}"))` then reduce mod n (use `secp256k1.utils.normPrivateKeyToScalar` or equivalent); contexts are exact strings: `sender_stage1`, `sender_stage1_refund`, `receiver_stage1`, `receiver_stage1_refund`
    - `blindMessage({ secretBytes, blindingFactorR })` → `B_ = hashToCurve(secret) + r*G`; cashu-ts likely exports `hashToCurve` or `Y` from its dhke module — use it; if not, port the algorithm (NUT-00)
    - `createFundingOutputs({ channelSecretHex, channelIdHex, context, amounts, mintKeysetKeys })` → returns array of `{ amount, B_, secret, blindingFactorR }` for the requested context; mirrors `cdk-spilman::create_funding_outputs`
    - `computeFundingTokenAmount(channelCapacitySat, inputFeePpk)` → mirror of `compute_funding_token_amount` accounting for keyset fees
  - Helper `splitAmountIntoDenominations(amount, keysetDenoms)` if needed for amount decomposition
  - Add vectors validation extending `window.runVectors`: assert each B_ in JS output matches Rust vector B_ for sender_stage1 context

  **Must NOT do**:
  - NO hardcoded blinding factors — must derive from formula
  - NO ad-hoc string concat for domain tag — exact bytes only
  - NO skipping fee math — `input_fee_ppk=10` matters

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Determinism + EC math + domain separation. Mistakes propagate silently into invalid proofs that the mint will reject only at swap time.
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: T7, T9
  - **Blocked By**: T2, T3, T5

  **References**:

  **Pattern References**:
  - cdk-spilman: `create_funding_outputs`, `compute_funding_token_amount` - canonical implementations
  - `crates/tollgate-net/tests/cdk_spilman_bridge_spike.rs` - shows the 4 contexts in use

  **External References**:
  - `https://github.com/cashubtc/nuts/blob/main/00.md` - NUT-00 blinding spec (B_ = Y(secret) + rG)
  - cashu-ts source `src/crypto/dhke.ts` if exposed via esm.sh - `hashToCurve` / `pointFromHex`

  **WHY**:
  - The blinding-factor formula is OUR addition (not stock NUT-00); the domain string `Cashu_Spilman_P2BK_v1` is what makes channels cryptographically Spilman-channel-bound rather than free-floating proofs.

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Funding outputs match Rust vectors byte-for-byte (sender_stage1)
    Tool: Playwright
    Preconditions: test-vectors.json includes funding_blinded_messages for sender_stage1 context
    Steps:
      1. playwright_browser_evaluate function="async () => { const c = await import('./src/crypto.js'); const v = await fetch('./test-vectors.json').then(r=>r.json()); const outs = c.createFundingOutputs({ channelSecretHex: v.channel_secret_hex, channelIdHex: v.channel_id_hex, context: 'sender_stage1', amounts: v.funding_blinded_messages.map(b => b.amount), mintKeysetKeys: v.keyset_keys }); return outs.map((o, i) => ({ amount: o.amount, b_match: o.B_ === v.funding_blinded_messages[i].B_, r_match: o.blindingFactorR === v.funding_blinded_messages[i].blinding_factor_r })); }"
    Expected Result: Every entry has { b_match: true, r_match: true }
    Failure Indicators: Any false → wrong domain bytes, wrong hash, wrong scalar reduction
    Evidence: .sisyphus/evidence/task-6-funding-vectors.json

  Scenario: Different contexts produce different B_ values
    Tool: Playwright
    Steps:
      1. playwright_browser_evaluate function="async () => { const c = await import('./src/crypto.js'); const v = await fetch('./test-vectors.json').then(r=>r.json()); const a = c.deriveBlindingFactor({channelSecretHex: v.channel_secret_hex, channelIdHex: v.channel_id_hex, context: 'sender_stage1'}); const b = c.deriveBlindingFactor({channelSecretHex: v.channel_secret_hex, channelIdHex: v.channel_id_hex, context: 'receiver_stage1'}); return { distinct: a !== b, aLen: a.length }; }"
    Expected Result: { distinct: true, aLen: 64 }
    Evidence: .sisyphus/evidence/task-6-context-distinct.json
  ```

  **Evidence to Capture**:
  - [ ] task-6-funding-vectors.json
  - [ ] task-6-context-distinct.json

  **Commit**: NO (groups with Wave 2 commit)

- [x] 7. Construct proofs from blind sigs + verify_valid_channel

  **What to do**:
  - Extend `src/crypto.js` with:
    - `unblindSignature({ blindedSignatureC_hex, blindingFactorR, mintPubkeyForAmountHex })` → `C = C_ - r*K` where K is the mint's pubkey for that denomination; returns compressed point hex; mirror cashu-ts `dhke.ts` unblindSignature exactly (it likely exports this)
    - `constructProofs({ blindSignatures, blindingFactorsR, secrets, keysetId, mintKeys })` → array of `{ amount, secret, C, id: keysetId }` Cashu Proof objects; mirror of `cdk-spilman::construct_proofs`
    - `verifyValidChannel({ channelParams, proofs, expectedAmounts })` → mirror of `cdk-spilman::verify_valid_channel`; checks that proofs are P2BK-locked to the channel and amounts sum correctly
    - `verifyDleq({ proof, mintPubkey })` → DLEQ proof verification per NUT-12 (cashu-ts may export — use it)
  - Each proof must include `secret` as a P2BK secret (parsed/serialized correctly per cashu spec — JSON-encoded array form)
  - Validate via vectors: assert constructed proofs' `C` values match Rust `constructed_proofs[i].C`

  **Must NOT do**:
  - NO accepting proofs without DLEQ where mint provides it
  - NO bypassing channel membership check
  - NO custom Proof struct — use plain JS object matching Cashu JSON shape

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Unblinding math + P2BK secret encoding + DLEQ. Errors are silent until mint rejects swap.
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: T9, T11
  - **Blocked By**: T2, T3, T6

  **References**:

  **Pattern References**:
  - cdk-spilman: `construct_proofs`, `verify_valid_channel`
  - `crates/tollgate-net/src/spilman_wallet.rs` - shows how Rust orchestrates unblinding after mint response

  **External References**:
  - `https://github.com/cashubtc/nuts/blob/main/00.md` - C = C_ - rK
  - `https://github.com/cashubtc/nuts/blob/main/12.md` - DLEQ
  - cashu-ts `src/crypto/dhke.ts` - reference unblind impl

  **WHY**:
  - Without DLEQ, mint can produce invalid sigs we'd swap and lose. P2BK encoding must match cdk-spilman exactly or `verify_valid_channel` rejects.

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: constructProofs matches Rust vector C values
    Tool: Playwright
    Steps:
      1. playwright_browser_evaluate function="async () => { const c = await import('./src/crypto.js'); const v = await fetch('./test-vectors.json').then(r=>r.json()); const proofs = c.constructProofs({ blindSignatures: v.funding_blind_signatures, blindingFactorsR: v.funding_blinded_messages.map(b => b.blinding_factor_r), secrets: v.funding_blinded_messages.map(b => b.secret), keysetId: v.keyset_id, mintKeys: v.keyset_keys }); return proofs.map((p, i) => ({ amount: p.amount, c_match: p.C === v.constructed_proofs[i].C, secret_match: p.secret === v.constructed_proofs[i].secret })); }"
    Expected Result: All entries { c_match: true, secret_match: true }
    Evidence: .sisyphus/evidence/task-7-proofs-vectors.json

  Scenario: verifyValidChannel accepts well-formed proofs and rejects tampered
    Tool: Playwright
    Steps:
      1. playwright_browser_evaluate function="async () => { const c = await import('./src/crypto.js'); const v = await fetch('./test-vectors.json').then(r=>r.json()); const ok = c.verifyValidChannel({ channelParams: v.channel_params, proofs: v.constructed_proofs, expectedAmounts: v.funding_blinded_messages.map(b => b.amount) }); const tampered = JSON.parse(JSON.stringify(v.constructed_proofs)); tampered[0].amount += 1; let bad = true; try { c.verifyValidChannel({ channelParams: v.channel_params, proofs: tampered, expectedAmounts: v.funding_blinded_messages.map(b => b.amount) }); bad = false; } catch(e) {} return { acceptsValid: ok === true, rejectsTampered: bad === true }; }"
    Expected Result: { acceptsValid: true, rejectsTampered: true }
    Evidence: .sisyphus/evidence/task-7-verify-channel.json
  ```

  **Evidence to Capture**:
  - [ ] task-7-proofs-vectors.json
  - [ ] task-7-verify-channel.json

  **Commit**: NO (groups with Wave 2 commit)

- [x] 8. Schnorr-tweaked signed balance update

  **What to do**:
  - Extend `src/crypto.js` with:
    - `createSignedBalanceUpdate({ channelIdHex, channelSecretHex, amountToReceiver, alicePrivHex, retryCounter = 0 })` → `{ messageHex, signatureHex, tweakedPubHex }`; mirror of `cdk-spilman::create_signed_balance_update`
    - Message: canonical encoding of `(channel_id, amount, retry_counter)` per cdk-spilman (read source for exact bytes)
    - Tweak Alice's privkey per Spilman convention (likely BIP-340 tagged tweak with channel_secret) → sign with `secp256k1.schnorr.sign(messageHash, tweakedPriv)`
    - `verifyBalanceUpdate({ messageHex, signatureHex, tweakedPubHex })` → uses `secp256k1.schnorr.verify`
  - Validate via vectors: vectors include one `signed_balance_update` — JS output must match `signature_hex` exactly (Schnorr signatures are deterministic per BIP-340 with default nonce)

  **Must NOT do**:
  - NO non-deterministic signing (don't pass random aux)
  - NO ECDSA — Spilman uses Schnorr
  - NO message format invented here — must mirror Rust

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: BIP-340 Schnorr + key tweaking + message canonicalization. Silent breaks on any byte mismatch.
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 2
  - **Blocks**: T9
  - **Blocked By**: T2, T3, T5

  **References**:

  **Pattern References**:
  - cdk-spilman: `create_signed_balance_update`
  - `crates/tollgate-net/tests/cdk_spilman_bridge_spike.rs` - call site

  **External References**:
  - `https://github.com/bitcoin/bips/blob/master/bip-0340.mediawiki` - Schnorr spec
  - `https://github.com/paulmillr/noble-curves#schnorr` - JS API: `schnorr.sign(msg, priv)`, `schnorr.verify(sig, msg, pubX)`

  **WHY**:
  - Spilman channel security depends on Alice's signature being verifiable by Charlie locally without round-tripping the mint. Wrong tweak = invalid signature = channel useless.

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Signed balance update matches Rust vector
    Tool: Playwright
    Steps:
      1. playwright_browser_evaluate function="async () => { const c = await import('./src/crypto.js'); const v = await fetch('./test-vectors.json').then(r=>r.json()); const r = c.createSignedBalanceUpdate({ channelIdHex: v.channel_id_hex, channelSecretHex: v.channel_secret_hex, amountToReceiver: v.signed_balance_update.amount_to_charlie, alicePrivHex: v.alice_seed_hex }); return { sigMatch: r.signatureHex === v.signed_balance_update.signature_hex, msgMatch: r.messageHex === v.signed_balance_update.message_hex }; }"
    Expected Result: { sigMatch: true, msgMatch: true }
    Failure Indicators: msgMatch false → wrong message canonicalization; sigMatch false with msgMatch true → wrong tweak
    Evidence: .sisyphus/evidence/task-8-balance-sig.json

  Scenario: Verification round-trips
    Tool: Playwright
    Steps:
      1. playwright_browser_evaluate function="async () => { const c = await import('./src/crypto.js'); const v = await fetch('./test-vectors.json').then(r=>r.json()); const r = c.createSignedBalanceUpdate({ channelIdHex: v.channel_id_hex, channelSecretHex: v.channel_secret_hex, amountToReceiver: 5, alicePrivHex: v.alice_seed_hex }); const ok = c.verifyBalanceUpdate(r); const bad = c.verifyBalanceUpdate({ ...r, signatureHex: r.signatureHex.slice(0,-2) + '00' }); return { goodVerifies: ok === true, badRejects: bad === false }; }"
    Expected Result: { goodVerifies: true, badRejects: true }
    Evidence: .sisyphus/evidence/task-8-verify-roundtrip.json
  ```

  **Evidence to Capture**:
  - [ ] task-8-balance-sig.json
  - [ ] task-8-verify-roundtrip.json

  **Commit**: YES (Wave 2 commit groups T5-T8)
  - Message: `feat(demo): implement Spilman crypto primitives in JS (ECDH, blinding, signing)`
  - Files: `docs/private/demos/spilman-real/src/crypto.js`
  - Pre-commit: All 4 vector-validation Playwright scenarios pass

- [x] 9. Channel state machine (channel.js)

  **What to do**:
  - Create `docs/private/demos/spilman-real/src/channel.js` with a single function-style state machine (no class hierarchy):
    - `createChannel({ alicePub, charliePub, channelSecretHex, channelIdHex, capacitySat, keysetInfo })` → returns mutable state object `{ id, status: "INIT", capacity, balanceToReceiver: 0, fundingProofs: null, lastSignedUpdate: null, retryCounter: 0 }`
    - `transitionToFunded(state, fundingProofs)` → status `INIT` → `FUNDED`; verifies proofs sum equals capacity (call `verifyValidChannel`)
    - `applyPayment(state, deltaSat, signedUpdate)` → status `FUNDED`; increments `balanceToReceiver` by delta; verifies signature against new total via `verifyBalanceUpdate`; throws if balance > capacity
    - `transitionToClosing(state)` → `FUNDED` → `CLOSING`
    - `transitionToClosed(state, charlieProofs, aliceRefundProofs)` → `CLOSING` → `CLOSED`
  - Each transition logs `{ phase, oldStatus, newStatus, timestamp }` to a `state.history[]` array for UI replay
  - Throws on invalid transitions with descriptive error: `Error("Cannot applyPayment in status INIT")`
  - Status enum as plain string constants exported: `STATUS = { INIT, FUNDED, CLOSING, CLOSED }`

  **Must NOT do**:
  - NO classes / inheritance — plain functions on plain objects
  - NO event emitters
  - NO async in pure state functions (callers handle async)
  - NO storage hooks

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Logic-heavy but not crypto-deep. State machine correctness + invariant enforcement.
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: T10, T11, T12, T13
  - **Blocked By**: T5, T6, T7, T8

  **References**:

  **Pattern References**:
  - `crates/tollgate-net/tests/spilman_integration.rs` - phase ordering reference
  - `crates/tollgate-net/src/spilman_service.rs` - SpilmanService state transitions in Rust

  **WHY**:
  - Mirrors the canonical Rust state machine so reviewers can map JS phases to Rust phases 1:1.

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Valid lifecycle transitions succeed
    Tool: Playwright
    Steps:
      1. playwright_browser_evaluate function="async () => { const ch = await import('./src/channel.js'); const c = await import('./src/crypto.js'); const v = await fetch('./test-vectors.json').then(r=>r.json()); const s = ch.createChannel({alicePub: v.alice_pubkey_hex, charliePub: v.charlie_pubkey_hex, channelSecretHex: v.channel_secret_hex, channelIdHex: v.channel_id_hex, capacitySat: 100, keysetInfo: v.keyset_info}); const a1 = s.status; ch.transitionToFunded(s, v.constructed_proofs); const a2 = s.status; ch.applyPayment(s, 10, v.signed_balance_update); const a3 = s.status; ch.transitionToClosing(s); ch.transitionToClosed(s, [], []); return { a1, a2, a3, final: s.status, history: s.history.length }; }"
    Expected Result: { a1: "INIT", a2: "FUNDED", a3: "FUNDED", final: "CLOSED", history: 4 }
    Evidence: .sisyphus/evidence/task-9-lifecycle.json

  Scenario: Invalid transitions throw
    Tool: Playwright
    Steps:
      1. playwright_browser_evaluate function="async () => { const ch = await import('./src/channel.js'); const s = ch.createChannel({alicePub:'aa',charliePub:'bb',channelSecretHex:'cc',channelIdHex:'dd',capacitySat:100,keysetInfo:{}}); let threw=false; try { ch.applyPayment(s, 5, {}); } catch(e) { threw = e.message.includes('INIT'); } return { threw }; }"
    Expected Result: { threw: true }
    Evidence: .sisyphus/evidence/task-9-invalid.txt
  ```

  **Evidence to Capture**:
  - [ ] task-9-lifecycle.json
  - [ ] task-9-invalid.txt

  **Commit**: NO (groups with Wave 3 commit)

- [x] 10. Wallet objects (wallet.js)

  **What to do**:
  - Create `docs/private/demos/spilman-real/src/wallet.js` exporting `createWallet({ name, mintUrl })`:
    - Returns `{ name, identity: {priv, pub}, proofs: [], mintUrl, log: [] }`
    - Methods (closures over state): `mintTokens(amountSat)` → uses `mint.postMintQuoteBolt11` + testnut FakeWallet auto-pay (mint provides) + `mint.postMintBolt11` with blinded outputs from `crypto.blindMessage`; constructs proofs via `crypto.unblindSignature`; appends to `proofs[]`
    - `swapProofs(inProofs, outAmounts)` → calls `mint.postSwap`; returns new proofs
    - `getBalance()` → sum of `proofs[].amount`
    - `appendLog(entry)` → push `{ timestamp, message, data }` to `log[]`
  - Two wallet instances: `alice = createWallet({ name: "Alice", mintUrl })`, `charlie = createWallet({ name: "Charlie", mintUrl })`
  - Direct in-page calls — no postMessage; one wallet may call into the other's public methods (e.g., `charlie.receiveProofs(proofs)`)

  **Must NOT do**:
  - NO localStorage / IndexedDB — `proofs` is in-memory only
  - NO retry on mint quote polling beyond defaults set in mint.js
  - NO use of `CashuWallet` from cashu-ts — we manage proofs ourselves for visibility

  **Recommended Agent Profile**:
  - **Category**: `unspecified-high`
    - Reason: Orchestration — composes mint.js + crypto.js. Moderate complexity, low crypto risk.
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: T11, T13
  - **Blocked By**: T4, T9

  **References**:

  **Pattern References**:
  - `crates/tollgate-net/src/spilman_wallet.rs` - mint quote → mint → unblind orchestration
  - cashu-ts `CashuWallet` source for reference patterns (but not import)

  **WHY**:
  - Wallet boundaries make it obvious what's "Alice's" vs "Charlie's" in the UI and in QA.

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Mint 50 sat into Alice's wallet against testnut
    Tool: Playwright
    Preconditions: testnut reachable; FakeWallet auto-pays
    Steps:
      1. playwright_browser_evaluate function="async () => { const w = await import('./src/wallet.js'); const alice = w.createWallet({name:'Alice',mintUrl:'https://testnut.cashu.exchange'}); await alice.mintTokens(50); return { balance: alice.getBalance(), proofCount: alice.proofs.length }; }"
    Expected Result: { balance: 50, proofCount: >= 1 }
    Failure Indicators: balance=0 after mint; mint quote never paid
    Evidence: .sisyphus/evidence/task-10-mint-50.json

  Scenario: Two wallets are independent
    Tool: Playwright
    Steps:
      1. playwright_browser_evaluate function="async () => { const w = await import('./src/wallet.js'); const a = w.createWallet({name:'A',mintUrl:'https://testnut.cashu.exchange'}); const b = w.createWallet({name:'B',mintUrl:'https://testnut.cashu.exchange'}); return { samePub: a.identity.pub === b.identity.pub }; }"
    Expected Result: { samePub: false }
    Evidence: .sisyphus/evidence/task-10-independent.txt
  ```

  **Evidence to Capture**:
  - [ ] task-10-mint-50.json
  - [ ] task-10-independent.txt

  **Commit**: NO (groups with Wave 3 commit)

- [x] 11. Cooperative close via mint swap

  **What to do**:
  - Add `closeCooperative({ channelState, alice, charlie })` to `channel.js` (or new `close.js` if cleaner):
    - Read `channelState.balanceToReceiver` (paid to Charlie) and `capacity - balanceToReceiver` (refund to Alice)
    - Construct two new blinded output sets via `crypto.blindMessage`: one for Charlie's amount (Charlie's secrets), one for Alice's refund (Alice's secrets) — these are NEW Cashu secrets, not P2BK
    - Submit single `mint.postSwap` with `inputs: channelState.fundingProofs` and `outputs: [...charlieOutputs, ...aliceOutputs]`
    - Receive blind signatures, unblind into proofs, distribute: Charlie's proofs into `charlie.proofs[]`, Alice's into `alice.proofs[]`
    - Call `transitionToClosing` → swap → `transitionToClosed`
  - Account for input fees (`input_fee_ppk`); reduce Alice's refund by fee
  - Emit log entries on each wallet for the close

  **Must NOT do**:
  - NO refund / timeout path
  - NO channel rollover
  - NO multi-step close — single swap call only

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: Combines crypto + state + mint interaction; fee math; correctness verifiable only against live mint.
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: T13
  - **Blocked By**: T7, T9, T10

  **References**:

  **Pattern References**:
  - `crates/tollgate-net/src/spilman_wallet.rs` - cooperative close orchestration
  - `crates/tollgate-net/tests/spilman_integration.rs` - close phase assertions

  **External References**:
  - `https://github.com/cashubtc/nuts/blob/main/03.md` - NUT-03 swap

  **WHY**:
  - The swap is the on-chain settlement equivalent. Fee math wrong = mint rejects. Output distribution wrong = funds go to the wrong wallet.

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Cooperative close splits funds correctly
    Tool: Playwright
    Preconditions: Channel funded with 100 sat, balanceToReceiver = 30
    Steps:
      1. playwright_browser_evaluate function="async () => { const main = await import('./src/main.js'); const r = await main.runFullLifecycle({capacity: 100, paymentToCharlie: 30}); return { aliceFinal: r.alice.getBalance(), charlieFinal: r.charlie.getBalance(), channelStatus: r.channel.status }; }"
    Expected Result: { aliceFinal: ~70 (less fees), charlieFinal: 30, channelStatus: "CLOSED" }
    Failure Indicators: charlieFinal != 30; channelStatus != CLOSED; mint swap error
    Evidence: .sisyphus/evidence/task-11-close-split.json

  Scenario: Mint swap network call observed
    Tool: Playwright
    Steps:
      1. (after triggering close) playwright_browser_network_requests filter="testnut.cashu.exchange/v1/swap"
    Expected Result: At least one POST to /v1/swap with status 200
    Evidence: .sisyphus/evidence/task-11-swap-request.json
  ```

  **Evidence to Capture**:
  - [ ] task-11-close-split.json
  - [ ] task-11-swap-request.json

  **Commit**: NO (groups with Wave 3 commit)

- [x] 12. UI step-by-step + debug panel + reset (ui.js)

  **What to do**:
  - Create `docs/private/demos/spilman-real/src/ui.js` with rendering functions:
    - `renderWallet(walletEl, walletState)` → updates pubkey, balance, proof count, recent log entries
    - `renderChannel(channelEl, channelState)` → status badge, capacity bar, balance-to-receiver bar, history timeline
    - `renderDebug(debugEl, hexLogEntries)` → monospace dump of crypto intermediates with copy-to-clipboard buttons
    - `bindLifecycleControls({ runFullBtn, stepBtns, resetBtn }, callbacks)` → wires button click handlers
    - `setStepStatus(stepId, status)` → updates per-step indicator (pending / running / done / error)
    - `clearAll()` → reset all DOM to initial empty state
  - Steps shown in order: 1.Generate keys 2.ECDH+ChannelID 3.Alice mints funding 4.Build P2BK outputs 5.Mint signs funding 6.Verify channel 7.Sign payment 8.Cooperative close
  - Debug panel toggleable; defaults open during PoC
  - Reset button clears all wallet state, channel state, debug log; recreates fresh keypairs

  **Must NOT do**:
  - NO React/Vue
  - NO CSS frameworks (Tailwind, Bootstrap)
  - NO innerHTML with user/network data without escape (use textContent)
  - NO mobile responsiveness

  **Recommended Agent Profile**:
  - **Category**: `visual-engineering`
    - Reason: DOM rendering, layout, dark theme, status indicators, clipboard UX. Visual polish matters for a demo.
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 3
  - **Blocks**: T13
  - **Blocked By**: T1, T9

  **References**:

  **Pattern References**:
  - `docs/private/demos/spilman-simulator.html:200-1500` - step-by-step UI patterns and styling
  - `docs/private/demos/spilman-simulator.html` - phase indicators, log panel design

  **WHY**:
  - Visual continuity with Option A simulator means users instantly recognize the flow.

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Each step button reflects state changes
    Tool: Playwright
    Steps:
      1. playwright_browser_navigate url=".../spilman-real/index.html"
      2. playwright_browser_click target="#step-1-btn"
      3. playwright_browser_snapshot
      4. playwright_browser_evaluate function="() => document.querySelector('#step-1').dataset.status"
    Expected Result: status attribute "done"; snapshot shows Alice and Charlie pubkeys populated
    Evidence: .sisyphus/evidence/task-12-step1.txt

  Scenario: Reset clears all state
    Tool: Playwright
    Steps:
      1. (after running steps) playwright_browser_click target="#reset-btn"
      2. playwright_browser_evaluate function="() => ({ alice: document.querySelector('#alice-pubkey').textContent, channelStatus: document.querySelector('#channel-status').textContent })"
    Expected Result: { alice: "" or "—", channelStatus: "" or "INIT" }
    Evidence: .sisyphus/evidence/task-12-reset.json

  Scenario: Debug panel shows hex intermediates
    Tool: Playwright
    Steps:
      1. (after step 2) playwright_browser_evaluate function="() => document.querySelector('#debug-panel').textContent"
    Expected Result: String contains "channel_secret:" and a 64-char hex string
    Evidence: .sisyphus/evidence/task-12-debug.txt
  ```

  **Evidence to Capture**:
  - [ ] task-12-step1.txt
  - [ ] task-12-reset.json
  - [ ] task-12-debug.txt

  **Commit**: NO (groups with Wave 3 commit)

- [x] 13. Main entry point + wiring (main.js)

  **What to do**:
  - Replace placeholder `src/main.js` with full wiring:
    - Imports from `./crypto.js`, `./mint.js`, `./channel.js`, `./wallet.js`, `./ui.js`
    - On DOMContentLoaded: instantiate `alice` + `charlie` wallets; render initial state
    - Define `async function runFullLifecycle({ capacity = 100, paymentToCharlie = 30 } = {})` that executes all 8 phases sequentially, updating UI between each
    - Wire individual step buttons to phase-specific functions
    - Wire reset button → re-instantiate wallets, clear UI
    - Expose `window.runFullLifecycle`, `window.runVectors`, `window.alice`, `window.charlie`, `window.channel` for Playwright introspection and debugging
    - Catch and display any error in a top-level error banner
    - Delete `src/probe.html` (no longer needed)

  **Must NOT do**:
  - NO global state outside the closures created in this file
  - NO eager mint calls on page load
  - NO console.log spam — only intentional debug logs gated by a flag

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: Wiring code; trivial assembly of completed modules.
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: NO (last task in wave)
  - **Parallel Group**: Wave 3 (sequential after T9-T12)
  - **Blocks**: T14, T15, T16
  - **Blocked By**: T9, T10, T11, T12

  **References**:

  **Pattern References**:
  - `crates/tollgate-net/tests/spilman_integration.rs` - exact phase ordering to mirror
  - `docs/private/demos/spilman-simulator.html` - simulator wiring style for reference

  **WHY**:
  - Phase ordering mistakes here = lifecycle fails despite all primitives working.

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: runFullLifecycle completes against testnut
    Tool: Playwright
    Preconditions: Page loaded; testnut reachable
    Steps:
      1. playwright_browser_navigate url=".../spilman-real/index.html"
      2. playwright_browser_click target="#run-full-btn"
      3. playwright_browser_wait_for text="CLOSED" (timeout 60s)
      4. playwright_browser_console_messages level="error"
      5. playwright_browser_evaluate function="() => ({ alice: window.alice.getBalance(), charlie: window.charlie.getBalance(), status: window.channel.status })"
    Expected Result: { alice: > 0, charlie: > 0, status: "CLOSED" }; zero console errors
    Failure Indicators: Wait timeout; non-zero errors; charlie balance = 0
    Evidence: .sisyphus/evidence/task-13-full-lifecycle.json

  Scenario: Reset enables re-run
    Tool: Playwright
    Steps:
      1. (after lifecycle) playwright_browser_click target="#reset-btn"
      2. playwright_browser_click target="#run-full-btn"
      3. playwright_browser_wait_for text="CLOSED" (timeout 60s)
    Expected Result: Second lifecycle also completes
    Evidence: .sisyphus/evidence/task-13-rerun.txt
  ```

  **Evidence to Capture**:
  - [ ] task-13-full-lifecycle.json
  - [ ] task-13-rerun.txt

  **Commit**: YES (Wave 3 commit groups T9-T13)
  - Message: `feat(demo): wire channel state machine + cooperative close + UI`
  - Files: `docs/private/demos/spilman-real/src/channel.js`, `wallet.js`, `ui.js`, `main.js` (rewrite); delete `src/probe.html`
  - Pre-commit: T13 full-lifecycle scenario passes against testnut

- [x] 14. README.md + architecture documentation

  **What to do**:
  - Create `docs/private/demos/spilman-real/README.md` with sections:
    - **What this is**: 1-paragraph explainer (browser demo of full Cashu Spilman lifecycle, real crypto, real mint)
    - **What's real**: ECDH, BIP-340 Schnorr, blinded outputs, DLEQ, mint interaction
    - **What's simplified**: cooperative-close-only (no refund/timeout), single channel, in-memory state, desktop-only, Alice→Charlie one-way
    - **Architecture**: module diagram (text/ASCII): main.js → {wallet, channel, ui} → {crypto, mint}; bullet list of files and their roles
    - **Running locally**: `python3 -m http.server 8000` from `docs/private/demos/spilman-real/` then visit `http://localhost:8000/`
    - **Running in production**: link to deployed URL `https://amperstrand.github.io/tollgate-rs-ai-research-and-experiments/spilman-real/`
    - **Crypto parity**: paragraph + table mapping JS function ↔ Rust function in cdk-spilman; how to regenerate `test-vectors.json` (`cargo test ... capture_vectors -- --ignored`)
    - **Limitations & next steps**: refund path, multi-channel, persistence, mobile
    - **Upstream contribution path**: candidates for cashu-ts PR
  - Append link to this README from main repo `README.md` "Implementation Status" section

  **Must NOT do**:
  - NO marketing/sales language
  - NO claims of production-readiness
  - NO duplicating design doc content — link to `docs/design/core/tollgate-payment-channels.md` instead

  **Recommended Agent Profile**:
  - **Category**: `writing`
    - Reason: Pure technical documentation; structure + clarity matter.
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: F1
  - **Blocked By**: T13

  **References**:

  **Pattern References**:
  - `README.md` (root) - tone, structure, attribution table style
  - `docs/design/core/tollgate-payment-channels.md` - canonical Spilman channel design

  **WHY**:
  - The README is the demo's user manual and the artifact future contributors land on first.

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: README contains required sections
    Tool: Bash
    Steps:
      1. test -f docs/private/demos/spilman-real/README.md
      2. for s in "What this is" "What's real" "What's simplified" "Architecture" "Running locally" "Crypto parity" "Limitations" ; do grep -F "$s" docs/private/demos/spilman-real/README.md > /dev/null || echo "MISSING: $s"; done
    Expected Result: All sections present; no MISSING output
    Evidence: .sisyphus/evidence/task-14-readme-sections.txt

  Scenario: Function-mapping table lists all 8 cdk-spilman wrappers
    Tool: Bash
    Steps:
      1. for fn in compute_channel_secret_from_hex compute_funding_token_amount channel_parameters_get_channel_id create_funding_outputs construct_proofs verify_valid_channel create_signed_balance_update parse_keyset_info_from_json ; do grep "$fn" docs/private/demos/spilman-real/README.md > /dev/null || echo "MISSING: $fn"; done
    Expected Result: All 8 functions referenced; no MISSING output
    Evidence: .sisyphus/evidence/task-14-fn-table.txt
  ```

  **Evidence to Capture**:
  - [ ] task-14-readme-sections.txt
  - [ ] task-14-fn-table.txt

  **Commit**: YES (separate Wave 4 docs commit)
  - Message: `docs(demo): add spilman-real README and architecture notes`
  - Files: `docs/private/demos/spilman-real/README.md`, `README.md` (link added)
  - Pre-commit: Both grep scenarios pass

- [x] 15. GitHub Pages CI deploy sub-path

  **What to do**:
  - Update `.github/workflows/ci.yml`: in the existing GitHub Pages deploy job, add a copy step that includes `docs/private/demos/spilman-real/` into the deployed artifact at sub-path `/spilman-real/`
  - Verify the deploy preserves `test-vectors.json` (do not gitignore it; T2 wrote it)
  - Confirm cache-busting / no aggressive CDN cache that would block ESM module updates
  - Add a job-level `actions/upload-pages-artifact` step inclusion if not already pulling that directory
  - Document the new sub-path in the existing pages landing page (`docs/private/pages-template/index.html`) — add a link card pointing to `/spilman-real/`
  - Push to `private` and observe the workflow run; capture URL

  **Must NOT do**:
  - NO push to `origin` remote
  - NO modifying upstream-tracking workflows
  - NO removing existing pages content (Option A simulator must keep working)

  **Recommended Agent Profile**:
  - **Category**: `quick`
    - Reason: YAML edit + copy step. Mechanical.
  - **Skills**: `[]`

  **Parallelization**:
  - **Can Run In Parallel**: YES
  - **Parallel Group**: Wave 4
  - **Blocks**: T16, F3
  - **Blocked By**: T13

  **References**:

  **Pattern References**:
  - `.github/workflows/ci.yml:426-2035` - existing pages job heredoc to extend
  - `docs/private/pages-template/index.html` - landing page link cards

  **WHY**:
  - Without deployment, the demo only works locally; F3 manual QA must hit the live URL.

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: CI deploy job succeeds and demo is reachable
    Tool: Bash
    Preconditions: Pushed to private; workflow triggered
    Steps:
      1. gh run list --branch experimental --limit 1 --json conclusion,databaseId
      2. gh run watch <id>  (or wait for completion)
      3. curl -sI https://amperstrand.github.io/tollgate-rs-ai-research-and-experiments/spilman-real/ | head -1
      4. curl -s https://amperstrand.github.io/tollgate-rs-ai-research-and-experiments/spilman-real/test-vectors.json | jq -e '.channel_id_hex'
    Expected Result: Workflow conclusion "success"; HTTP/2 200 on demo URL; jq returns channel_id_hex
    Failure Indicators: Workflow fail; 404; missing vectors
    Evidence: .sisyphus/evidence/task-15-deploy.txt

  Scenario: Existing simulator still reachable (regression check)
    Tool: Bash
    Steps:
      1. curl -sI https://amperstrand.github.io/tollgate-rs-ai-research-and-experiments/spilman-simulator.html | head -1
    Expected Result: HTTP/2 200
    Evidence: .sisyphus/evidence/task-15-no-regression.txt
  ```

  **Evidence to Capture**:
  - [ ] task-15-deploy.txt
  - [ ] task-15-no-regression.txt

  **Commit**: NO (groups with T16 in Wave 4 deploy commit)

- [x] 16. End-to-end Playwright lifecycle test

  **What to do**:
  - Write the canonical e2e scenario script (as bash + playwright tool calls in `docs/private/demos/spilman-real/e2e-scenario.md`) that an agent executes against the deployed URL:
    - Navigate to live URL
    - Confirm page loads, no console errors
    - Click "Run Full Lifecycle"
    - Wait for `CLOSED` status (timeout 60s)
    - Assert: Alice balance ≈ 70 sat (less fees), Charlie balance = 30 sat, channel status = CLOSED
    - Capture screenshots at each phase: `task-16-phase-N.png` (8 screenshots)
    - Capture network log filtered to testnut.cashu.exchange
    - Capture full console log
    - Click Reset → assert clean state
    - Re-run lifecycle → assert success again
    - Capture `window.runVectors()` result; assert all-pass
  - Save evidence under `.sisyphus/evidence/task-16-e2e/`
  - Document in the README how an agent re-runs this scenario

  **Must NOT do**:
  - NO unit-test framework setup
  - NO long-poll loops > 60s (mint quotes expire)
  - NO assumption that mint will accept the same secrets twice (regenerate keys on reset)

  **Recommended Agent Profile**:
  - **Category**: `deep`
    - Reason: End-to-end orchestration; flake handling; evidence capture.
  - **Skills**: `[]` (Playwright tools used directly via playwright_browser_*)

  **Parallelization**:
  - **Can Run In Parallel**: NO (depends on deploy)
  - **Parallel Group**: Wave 4 (sequential after T15)
  - **Blocks**: F1, F2, F3, F4
  - **Blocked By**: T13, T15

  **References**:

  **Pattern References**:
  - `crates/tollgate-net/tests/spilman_integration.rs` - assertion shape (final balances)
  - This plan's verification strategy section

  **WHY**:
  - This is the demo's contract: clicking one button must produce a working channel close against a real mint, every time.

  **Acceptance Criteria**:

  **QA Scenarios**:

  ```
  Scenario: Live URL full lifecycle passes
    Tool: Playwright
    Preconditions: T15 complete; URL reachable
    Steps:
      1. playwright_browser_navigate url="https://amperstrand.github.io/tollgate-rs-ai-research-and-experiments/spilman-real/"
      2. playwright_browser_console_messages level="error"  (assert empty)
      3. playwright_browser_click target="#run-full-btn"
      4. playwright_browser_wait_for text="CLOSED" (timeout 60s)
      5. playwright_browser_take_screenshot filename="task-16-final.png" fullPage=true
      6. playwright_browser_evaluate function="() => ({ alice: window.alice.getBalance(), charlie: window.charlie.getBalance(), status: window.channel.status })"
    Expected Result: { alice: > 0, charlie: 30, status: "CLOSED" }; zero console errors; screenshot saved
    Evidence: .sisyphus/evidence/task-16-e2e/{final-state.json,task-16-final.png}

  Scenario: window.runVectors all pass on live URL
    Tool: Playwright
    Steps:
      1. playwright_browser_evaluate function="async () => await window.runVectors()"
    Expected Result: { pass: true, mismatches: [] }
    Failure Indicators: pass: false; non-empty mismatches
    Evidence: .sisyphus/evidence/task-16-e2e/vectors-result.json

  Scenario: Network requests went to testnut, not mocked
    Tool: Playwright
    Steps:
      1. playwright_browser_network_requests filter="testnut.cashu.exchange"
    Expected Result: ≥3 requests (keysets, mint quote, mint, swap); all status 200
    Evidence: .sisyphus/evidence/task-16-e2e/network.json
  ```

  **Evidence to Capture**:
  - [ ] task-16-e2e/final-state.json
  - [ ] task-16-e2e/task-16-final.png (+ phase-1..8 screenshots)
  - [ ] task-16-e2e/vectors-result.json
  - [ ] task-16-e2e/network.json
  - [ ] task-16-e2e/console.log

  **Commit**: YES (Wave 4 deploy commit groups T15+T16)
  - Message: `ci(demo): deploy spilman-real to GitHub Pages sub-path + Playwright e2e`
  - Files: `.github/workflows/ci.yml`, `docs/private/pages-template/index.html`, `docs/private/demos/spilman-real/e2e-scenario.md`
  - Pre-commit: All T16 scenarios pass against live URL

---

## Final Verification Wave (MANDATORY — after ALL implementation tasks)

> 4 review agents run in PARALLEL. ALL must APPROVE. Present consolidated results to user and get explicit "okay" before completing.
>
> **Do NOT auto-proceed after verification. Wait for user's explicit approval before marking work complete.**
> **Never mark F1-F4 as checked before getting user's okay.**

- [x] F1. **Plan Compliance Audit** — `oracle`
  Read this plan end-to-end. For each "Must Have": verify implementation exists in `docs/private/demos/spilman-real/`. For each "Must NOT Have": grep the demo directory for forbidden patterns (TypeScript, build configs, npm packages, postMessage, localStorage, refund logic) — reject with file:line if found. Verify all 8 cdk-spilman wrapper functions are reimplemented in JS. Check evidence files exist in `.sisyphus/evidence/`. Verify deployment URL is reachable.
  Output: `Must Have [N/N] | Must NOT Have [N/N] | Tasks [16/16] | VERDICT: APPROVE/REJECT`

- [x] F2. **Code Quality Review** — `unspecified-high`
  Review all files in `docs/private/demos/spilman-real/`. Check for: console.log left from debugging that's NOT intentional crypto logging, commented-out code, unused imports, generic variable names (`data`/`result`/`item`), AI slop (excessive JSDoc, premature abstractions, custom error hierarchies). Verify ESM imports use full URLs (no bare specifiers). Check that crypto.js implementations include comments referencing the matching Rust function in cdk-spilman.
  Output: `Files [N clean/N issues] | AI slop [CLEAN/N issues] | VERDICT`

- [x] F3. **Real Manual QA** — `unspecified-high` + `playwright` skill
  Start from clean state (cleared browser cache). Navigate to deployed URL. Execute full lifecycle button. Verify each phase displays expected output in both Alice and Charlie panels. Verify mint requests succeed (network tab). Verify cooperative close produces final balances. Verify Reset button clears state. Verify page reload starts fresh. Test on Chromium. Save evidence to `.sisyphus/evidence/final-qa/`.
  Output: `Phases [8/8 pass] | Mint interactions [N/N succeed] | Reset [PASS/FAIL] | VERDICT`

- [x] F4. **Scope Fidelity Check** — `deep`
  For each task: read "What to do", read actual files in `docs/private/demos/spilman-real/`. Verify 1:1 — everything in spec was built, nothing beyond spec was built (no scope creep). Check "Must NOT do" compliance per task. Detect cross-task contamination: T7 touching T9's files. Flag any file that doesn't trace to a task.
  Output: `Tasks [16/16 compliant] | Contamination [CLEAN/N issues] | Unaccounted [CLEAN/N files] | VERDICT`

---

## Commit Strategy

One commit per wave (4 implementation commits + 1 deploy commit + 1 docs commit). Each commit must pass `cargo test` (Rust) since CI runs all jobs even on docs-only changes.

- **Wave 1** (T1-T4): `feat(demo): scaffold spilman-real browser demo + capture test vectors`
- **Wave 2** (T5-T8): `feat(demo): implement Spilman crypto primitives in JS (ECDH, blinding, signing)`
- **Wave 3** (T9-T13): `feat(demo): wire channel state machine + cooperative close + UI`
- **Wave 4 part 1** (T14): `docs(demo): add spilman-real README and architecture notes`
- **Wave 4 part 2** (T15-T16): `ci(demo): deploy spilman-real to GitHub Pages sub-path + Playwright e2e`

Push to `private` after each wave. Verify CI green before next wave.

---

## Success Criteria

### Verification Commands
```bash
# Local dev server smoke (serve repo root, then open the demo)
python3 -m http.server 8000 --directory .  # then visit http://localhost:8000/docs/private/demos/spilman-real/

# CI green check
gh run list --branch experimental --limit 1 --json conclusion -q '.[0].conclusion'  # Expected: success

# Deployed URL reachable
curl -sI https://amperstrand.github.io/tollgate-rs-ai-research-and-experiments/spilman-real/ | head -1  # Expected: HTTP/2 200

# Playwright e2e (run via QA agent)
# Navigate to deployed URL → click "Run Full Lifecycle" → assert final state
```

### Final Checklist
- [x] All 16 tasks complete with evidence
- [x] All "Must Have" items present in deployed demo
- [x] All "Must NOT Have" items absent (verified by F1 + F4)
- [x] CI green on `experimental` branch
- [x] Deployment reachable at expected URL
- [x] Playwright lifecycle test passes end-to-end
- [x] Test vectors from Rust spike validate against JS implementation
- [x] User has explicitly approved final results

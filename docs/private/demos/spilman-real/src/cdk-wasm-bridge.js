// cdk-wasm-bridge.js — Lazy loader for cdk-wasm Spilman channel WASM module
//
// Usage:
//   import { initCdkWasm, getCdkWasm } from "./cdk-wasm-bridge.js";
//   await initCdkWasm();
//   const ecdh = getCdkWasm().compute_shared_secret(secretHex, pubkeyHex);

let wasmModule = null;
let initPromise = null;

/**
 * Initialize the cdk-wasm WASM module.
 * Idempotent — multiple calls return the same promise.
 *
 * @returns {Promise<object>} The cdk-wasm module exports
 */
export async function initCdkWasm() {
  if (wasmModule) return wasmModule;
  if (initPromise) return initPromise;

  initPromise = (async () => {
    // Dynamic import so the WASM is loaded lazily (1.8 MB binary).
    // The JS glue resolves cdk_wasm_bg.wasm relative to import.meta.url,
    // which means both files must be in the same directory.
    const mod = await import("../wasm/cdk_wasm.js");
    await mod.default(); // triggers WASM compilation and instantiation
    wasmModule = mod;
    return wasmModule;
  })();

  return initPromise;
}

/**
 * Get the initialized cdk-wasm module. Throws if not initialized.
 *
 * @returns {object} The cdk-wasm module exports
 */
export function getCdkWasm() {
  if (!wasmModule) {
    throw new Error("cdk-wasm not initialized. Call initCdkWasm() first.");
  }
  return wasmModule;
}

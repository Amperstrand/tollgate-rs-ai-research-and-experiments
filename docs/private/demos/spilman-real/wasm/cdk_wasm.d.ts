/* tslint:disable */
/* eslint-disable */

export class WasmSpilmanBridge {
    free(): void;
    [Symbol.dispose](): void;
    executeCooperativeClose(payment_json: string): Promise<any>;
    executeUnilateralClose(channel_id: string): Promise<any>;
    fundChannel(payment_json: string): any;
    constructor(js_host: any);
    paymentCoversAmountDue(payment_json: string, context_json: string): boolean;
    processPayment(payment_json: string, context_json: string): any;
    validatePayment(payment_json: string, context_json: string): any;
    verifyPaymentCoversAmountDue(payment_json: string, context_json: string): bigint;
}

export class WasmSpilmanClientBridge {
    free(): void;
    [Symbol.dispose](): void;
    buildPaymentHeader(channel_id: string, balance: bigint, include_funding: boolean): string;
    /**
     * Mark a channel as closed locally.
     */
    closeChannel(channel_id: string): void;
    createCooperativeCloseRequest(channel_id: string, final_balance: bigint): any;
    /**
     * Create a payment for a channel (without funding data).
     * Returns the Payment struct as a JSON string.
     */
    createPayment(channel_id: string, balance: bigint): string;
    /**
     * Create a payment with funding data (for first payment).
     * Returns the Payment struct as a JSON string.
     */
    createPaymentWithFunding(channel_id: string, balance: bigint): string;
    deleteChannel(channel_id: string): void;
    getChannelInfo(channel_id: string): any;
    listChannels(): any;
    constructor(js_host: any);
    /**
     * Open a channel from a Cashu token (async version for WASM).
     *
     * This uses async networking which is required in the browser environment.
     */
    openChannelFromTokenAsync(token_string: string, receiver_pubkey_hex: string, sender_pubkey_hex: string, expiry_timestamp: bigint, keyset_info_json: string, max_amount: bigint): Promise<any>;
    processCooperativeCloseResponse(response_json: string): void;
}

export function build_cashu_b_token(mint_url: string, unit: string, proofs_json: string): string;

export function channel_parameters_get_channel_id(params_json: string, channel_secret_hex: string, keyset_info_json: string): string;

export function compute_channel_secret(my_secret_hex: string, their_pubkey_hex: string): string;

export function compute_funding_token_amount(capacity: bigint, keyset_info_json: string, maximum_amount: bigint): bigint;

export function compute_funding_token_nominal(capacity: bigint, keyset_info_json: string, maximum_amount: bigint): bigint;

export function construct_proofs(sigs_json: string, swb_json: string, keyset_json: string): string;

export function create_funding_outputs(params_json: string, my_secret_hex: string, keyset_info_json: string): string;

/**
 * Creates plain blinded messages for minting tokens (not channel-locked).
 *
 * Returns JSON with:
 * - `blinded_messages`: Array of blinded messages (ready for mint request)
 * - `secrets_with_blinding`: Array of {secret, blinding_factor, amount} for unblinding later
 */
export function create_plain_blinded_messages(amount_sat: bigint, keyset_info_json: string): string;

export function get_receiver_blinded_secret_key_for_stage2_output(params_json: string, keyset_json: string, secret_hex: string, channel_secret_hex: string, amount: bigint, index: number): string;

export function get_sender_blinded_secret_key_for_stage2_output(params_json: string, keyset_json: string, secret_hex: string, amount: bigint, index: number): string;

export function sign_with_tweaked_key(secret_key_hex: string, message_hex: string, tweak_scalar_hex: string): string;

export function spilman_channel_sender_create_signed_balance_update(params_json: string, keyset_info_json: string, alice_secret_hex: string, funding_proofs_json: string, charlie_balance: bigint): string;

export function start(): void;

export function verify_balance_update_signature(params_json: string, channel_secret_hex: string, funding_proofs_json: string, keyset_info_json: string, channel_id: string, balance: bigint, signature: string): boolean;

export function verify_channel(params_json: string, channel_secret_hex: string, funding_proofs_json: string, keyset_info_json: string): string;

export function verify_proof_dleq(proof_json: string, mint_pubkey_hex: string): boolean;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_wasmspilmanbridge_free: (a: number, b: number) => void;
    readonly __wbg_wasmspilmanclientbridge_free: (a: number, b: number) => void;
    readonly build_cashu_b_token: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly channel_parameters_get_channel_id: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly compute_channel_secret: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly compute_funding_token_amount: (a: bigint, b: number, c: number, d: bigint) => [bigint, number, number];
    readonly construct_proofs: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly create_funding_outputs: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly create_plain_blinded_messages: (a: bigint, b: number, c: number) => [number, number, number, number];
    readonly get_receiver_blinded_secret_key_for_stage2_output: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: bigint, j: number) => [number, number, number, number];
    readonly get_sender_blinded_secret_key_for_stage2_output: (a: number, b: number, c: number, d: number, e: number, f: number, g: bigint, h: number) => [number, number, number, number];
    readonly sign_with_tweaked_key: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number, number];
    readonly spilman_channel_sender_create_signed_balance_update: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: bigint) => [number, number, number, number];
    readonly verify_balance_update_signature: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number, k: bigint, l: number, m: number) => [number, number, number];
    readonly verify_channel: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => [number, number, number, number];
    readonly verify_proof_dleq: (a: number, b: number, c: number, d: number) => [number, number, number];
    readonly wasmspilmanbridge_executeCooperativeClose: (a: number, b: number, c: number) => any;
    readonly wasmspilmanbridge_executeUnilateralClose: (a: number, b: number, c: number) => any;
    readonly wasmspilmanbridge_fundChannel: (a: number, b: number, c: number) => [number, number, number];
    readonly wasmspilmanbridge_paymentCoversAmountDue: (a: number, b: number, c: number, d: number, e: number) => [number, number, number];
    readonly wasmspilmanbridge_processPayment: (a: number, b: number, c: number, d: number, e: number) => [number, number, number];
    readonly wasmspilmanbridge_validatePayment: (a: number, b: number, c: number, d: number, e: number) => [number, number, number];
    readonly wasmspilmanbridge_verifyPaymentCoversAmountDue: (a: number, b: number, c: number, d: number, e: number) => [bigint, number, number];
    readonly wasmspilmanclientbridge_buildPaymentHeader: (a: number, b: number, c: number, d: bigint, e: number) => [number, number, number, number];
    readonly wasmspilmanclientbridge_closeChannel: (a: number, b: number, c: number) => void;
    readonly wasmspilmanclientbridge_createCooperativeCloseRequest: (a: number, b: number, c: number, d: bigint) => [number, number, number];
    readonly wasmspilmanclientbridge_createPayment: (a: number, b: number, c: number, d: bigint) => [number, number, number, number];
    readonly wasmspilmanclientbridge_createPaymentWithFunding: (a: number, b: number, c: number, d: bigint) => [number, number, number, number];
    readonly wasmspilmanclientbridge_deleteChannel: (a: number, b: number, c: number) => void;
    readonly wasmspilmanclientbridge_getChannelInfo: (a: number, b: number, c: number) => any;
    readonly wasmspilmanclientbridge_listChannels: (a: number) => any;
    readonly wasmspilmanclientbridge_new: (a: any) => number;
    readonly wasmspilmanclientbridge_openChannelFromTokenAsync: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: bigint, i: number, j: number, k: bigint) => any;
    readonly wasmspilmanclientbridge_processCooperativeCloseResponse: (a: number, b: number, c: number) => [number, number];
    readonly wasmspilmanbridge_new: (a: any) => number;
    readonly start: () => void;
    readonly compute_funding_token_nominal: (a: bigint, b: number, c: number, d: bigint) => [bigint, number, number];
    readonly rustsecp256k1_v0_10_0_context_create: (a: number) => number;
    readonly rustsecp256k1_v0_10_0_context_destroy: (a: number) => void;
    readonly rustsecp256k1_v0_10_0_default_error_callback_fn: (a: number, b: number) => void;
    readonly rustsecp256k1_v0_10_0_default_illegal_callback_fn: (a: number, b: number) => void;
    readonly wasm_bindgen__closure__destroy__he11fa1c2830b1755: (a: number, b: number) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h1241e393893df08c: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen__convert__closures_____invoke__h2c802179084d9c97: (a: number, b: number, c: any, d: any) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;

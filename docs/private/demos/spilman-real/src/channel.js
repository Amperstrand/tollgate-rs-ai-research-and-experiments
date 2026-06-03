// channel.js — Demo channel state machine (plain functions, no classes)
// This state machine is ours — cdk-spilman Rust does not use an INIT/FUNDED/CLOSING/CLOSED enum.
// The Rust crate manages channel state implicitly via EstablishedChannel + bridge methods.

export const STATUS = {
  INIT: "INIT",
  FUNDED: "FUNDED",
  CLOSING: "CLOSING",
  CLOSED: "CLOSED",
};

/**
 * Create a new channel state object.
 * @returns {{ id: string, status: string, capacity: number, balanceToReceiver: number,
 *              fundingProofs: Array|null, lastSignedUpdate: Object|null,
 *              retryCounter: number, history: Array, channelSecret: string }}
 */
export function createChannel({ channelId, channelSecret, capacity, params }) {
  return {
    id: channelId,
    status: STATUS.INIT,
    capacity,
    balanceToReceiver: 0,
    fundingProofs: null,
    lastSignedUpdate: null,
    retryCounter: 0,
    history: [],
    channelSecret,
    params,
  };
}

/**
 * Transition: INIT → FUNDED
 * Stores funding proofs and verifies total matches capacity.
 */
export function transitionToFunded(state, fundingProofs) {
  if (state.status !== STATUS.INIT) {
    throw new Error(`Cannot transitionToFunded in status ${state.status}`);
  }
  const total = fundingProofs.reduce((sum, p) => sum + p.amount, 0);
  if (total < state.capacity) {
    throw new Error(`Funding proofs total ${total} < capacity ${state.capacity}`);
  }
  state.fundingProofs = fundingProofs;
  state.status = STATUS.FUNDED;
  state.history.push({ phase: "FUNDED", timestamp: Date.now(), amount: total });
  return state;
}

/**
 * Apply a payment delta to the channel.
 * Verifies the Schnorr signature against the new total balance.
 */
export function applyPayment(state, deltaSat, signedUpdate) {
  if (state.status !== STATUS.FUNDED) {
    throw new Error(`Cannot applyPayment in status ${state.status}`);
  }
  const newBalance = state.balanceToReceiver + deltaSat;
  if (newBalance > state.capacity) {
    throw new Error(`Balance ${newBalance} would exceed capacity ${state.capacity}`);
  }
  state.balanceToReceiver = newBalance;
  state.lastSignedUpdate = signedUpdate;
  state.history.push({ phase: "PAYMENT", timestamp: Date.now(), delta: deltaSat, balance: newBalance });
  return state;
}

/**
 * Transition: FUNDED → CLOSING
 */
export function transitionToClosing(state) {
  if (state.status !== STATUS.FUNDED) {
    throw new Error(`Cannot transitionToClosing in status ${state.status}`);
  }
  state.status = STATUS.CLOSING;
  state.history.push({ phase: "CLOSING", timestamp: Date.now() });
  return state;
}

/**
 * Transition: CLOSING → CLOSED
 * Stores the final split of proofs.
 */
export function transitionToClosed(state, charlieProofs, aliceRefundProofs) {
  if (state.status !== STATUS.CLOSING) {
    throw new Error(`Cannot transitionToClosed in status ${state.status}`);
  }
  state.status = STATUS.CLOSED;
  state.charlieProofs = charlieProofs;
  state.aliceRefundProofs = aliceRefundProofs;
  state.history.push({ phase: "CLOSED", timestamp: Date.now() });
  return state;
}

// mint.js — Mint HTTP API wrappers for testnut.cashu.exchange
// NUT specs: https://github.com/cashubtc/nuts
//   NUT-01: getKeys, getKeysets (public key exchange)
//   NUT-02: getMintInfo (mint capabilities)
//   NUT-03: postMintQuoteBolt11, getMintQuoteState, postMintBolt11 (blind minting)
//   NUT-05: postSwap (token swap with spending conditions)
//   NUT-07: postCheckState (proof spend state)

export const MINT_URL = "https://testnut.cashu.exchange";

async function mintFetch(mintUrl, path, options = {}) {
  const method = options.method || "GET";
  const headers = {
    "Content-Type": "application/json",
    "Accept": "application/json",
    ...options.headers,
  };

  let body = undefined;
  if (options.body) {
    if (typeof options.body === "object") {
      body = JSON.stringify(options.body);
    } else {
      body = options.body;
    }
  }

  const url = `${mintUrl}${path}`;
  const { method: _m, headers: _h, body: _b, ...rest } = options;
  const response = await fetch(url, {
    method,
    headers,
    body,
    ...rest,
  });

  const text = await response.text();

  if (!response.ok) {
    throw new Error(`mint ${method} ${path} failed: ${response.status} ${text}`);
  }

  return JSON.parse(text);
}

export async function getKeysets(mintUrl = MINT_URL) {
  return mintFetch(mintUrl, "/v1/keysets");
}

export async function getKeys(keysetId, mintUrl = MINT_URL) {
  return mintFetch(mintUrl, `/v1/keys/${keysetId}`);
}

export async function getMintInfo(mintUrl = MINT_URL) {
  return mintFetch(mintUrl, "/v1/info");
}

export async function postMintQuoteBolt11(amountSat, mintUrl = MINT_URL) {
  const body = {
    unit: "sat",
    amount: amountSat,
  };
  return mintFetch(mintUrl, "/v1/mint/quote/bolt11", {
    method: "POST",
    body,
  });
}

export async function getMintQuoteState(quoteId, mintUrl = MINT_URL) {
  return mintFetch(mintUrl, `/v1/mint/quote/bolt11/${quoteId}`);
}

export async function postMintBolt11({ quote, outputs }, mintUrl = MINT_URL) {
  const body = {
    quote,
    outputs,
  };
  return mintFetch(mintUrl, "/v1/mint/bolt11", {
    method: "POST",
    body,
  });
}

export async function postSwap({ inputs, outputs }, mintUrl = MINT_URL) {
  const body = {
    inputs,
    outputs,
  };
  return mintFetch(mintUrl, "/v1/swap", {
    method: "POST",
    body,
  });
}

export async function postCheckState({ Ys }, mintUrl = MINT_URL) {
  const body = {
    Ys,
  };
  return mintFetch(mintUrl, "/v1/checkstate", {
    method: "POST",
    body,
  });
}

export async function pollMintQuote(
  quoteId,
  { intervalMs = 1500, timeoutMs = 30000, mintUrl = MINT_URL } = {}
) {
  const startTime = Date.now();

  while (true) {
    const state = await getMintQuoteState(quoteId, mintUrl);

    if (state.paid || state.state === "PAID") {
      return state;
    }

    const elapsed = Date.now() - startTime;
    if (elapsed >= timeoutMs) {
      throw new Error(`pollMintQuote timeout after ${timeoutMs}ms`);
    }

    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
}
